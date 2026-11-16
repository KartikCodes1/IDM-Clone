//! Property and concurrency tests of the segment table (PLAN S1.11, ADR-010): the ordering
//! invariant and the tiling hold after any sequence of operations, and a shrunk worker can
//! never take a byte past its end, even with many threads splitting at once.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::sync::{Barrier, Mutex};

use forgedm_core::transfer::segment_table::{
    MAX_SEGMENTS, MIN_SPLIT, SPLIT_ALIGN, SegmentTable, WorkerId,
};
use proptest::prelude::*;

const MIB: u64 = 1024 * 1024;
const WORKERS: usize = 6;

#[derive(Debug, Clone)]
enum Op {
    Assign(usize),
    Claim(usize, u64),
    Send(usize),
    Write(usize),
    Flush,
    Commit,
    Release(usize),
    Speed(usize, u32),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..WORKERS).prop_map(Op::Assign),
        6 => (0..WORKERS, 1..(3 * MIB)).prop_map(|(w, n)| Op::Claim(w, n)),
        4 => (0..WORKERS).prop_map(Op::Send),
        4 => (0..WORKERS).prop_map(Op::Write),
        2 => Just(Op::Flush),
        2 => Just(Op::Commit),
        2 => (0..WORKERS).prop_map(Op::Release),
        1 => (0..WORKERS, 1..20_000_000u32).prop_map(|(w, s)| Op::Speed(w, s)),
    ]
}

fn worker_id(index: usize) -> WorkerId {
    u32::try_from(index).unwrap() + 1
}

/// Applies `op`; `held[w]` is the segment worker `w` was assigned.
fn apply(table: &mut SegmentTable, held: &mut [Option<u32>; WORKERS], op: &Op) {
    match *op {
        Op::Assign(w) if held[w].is_none() => {
            held[w] = table.assign(worker_id(w)).map(|a| a.seg_no);
        }
        Op::Claim(w, n) => {
            if let Some(no) = held[w] {
                let seg = table.segment(no).unwrap().clone();
                let allowed = table.claim(no, worker_id(w), seg.claim, n).unwrap();
                assert_eq!(allowed, n.min(seg.end - seg.claim));
            }
        }
        Op::Send(w) => {
            if let Some(no) = held[w] {
                let claim = table.segment(no).unwrap().claim;
                table.mark_sent(no, claim).unwrap();
            }
        }
        Op::Write(w) => {
            if let Some(no) = held[w] {
                let seg = table.segment(no).unwrap().clone();
                table.note_written(seg.written, seg.sent - seg.written);
            }
        }
        Op::Flush => {
            let frontier = table.written_frontier();
            table.note_flushed(&frontier);
        }
        Op::Commit => {
            let view = table.checkpoint_view();
            let committed = table.apply_committed(&view);
            // What is persisted must be a valid table that restores to the same bytes.
            let restored = SegmentTable::from_snapshots(&view.records, table.total()).unwrap();
            assert_eq!(restored.committed_bytes(), committed);
        }
        Op::Release(w) => {
            if let Some(no) = held[w].take() {
                table.release(no, worker_id(w));
            }
        }
        Op::Speed(w, bps) => {
            if let Some(no) = held[w] {
                table.set_speed(no, f64::from(bps));
            }
        }
        Op::Assign(_) => {}
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn proptest_table_invariants(
        total in (MIB / 2)..(300 * MIB),
        conns in 1u32..=8,
        ops in proptest::collection::vec(op(), 1..250),
    ) {
        let mut table = SegmentTable::plan(total, conns);
        table.check_invariants().unwrap();
        let mut held = [None; WORKERS];
        let (mut accepted, mut committed) = (0, 0);
        for step in &ops {
            apply(&mut table, &mut held, step);
            prop_assert_eq!(table.check_invariants(), Ok(()), "after {:?}", step);
            prop_assert!(table.segments().len() <= MAX_SEGMENTS);
            prop_assert!(table.accepted_bytes() >= accepted, "accepted never decreases");
            prop_assert!(table.committed_bytes() >= committed, "committed never decreases");
            prop_assert!(table.committed_bytes() <= table.accepted_bytes());
            accepted = table.accepted_bytes();
            committed = table.committed_bytes();
        }
    }

    #[test]
    fn every_split_leaves_aligned_halves_of_at_least_min_split(
        total in (16 * MIB)..(400 * MIB),
        claimed in 0..(8 * MIB),
    ) {
        let mut table = SegmentTable::plan(total, 1);
        let first = table.assign(1).unwrap();
        table.claim(first.seg_no, 1, 0, claimed.min(total)).unwrap();
        let before = table.segments()[0].clone();

        if let Some(stolen) = table.assign(2) {
            let victim = &table.segments()[0];
            prop_assert_eq!(stolen.claim, victim.end, "the new segment starts at the victim's new end");
            prop_assert_eq!(stolen.claim % SPLIT_ALIGN, 0);
            prop_assert!(victim.end - before.claim >= MIN_SPLIT);
            prop_assert!(stolen.end - stolen.claim >= MIN_SPLIT);
        }
        prop_assert_eq!(table.check_invariants(), Ok(()));
    }
}

/// A tiny deterministic generator so the stress test needs no extra crate.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// One thread's loop: take a segment, claim chunks until it ends or shrinks, repeat.
/// Returns every `(position, length)` it claimed (and sent immediately).
///
/// Every worker takes its first assignment and then waits at `start`, so no byte is claimed
/// until all workers hold a segment. Workers beyond the planned segment count therefore
/// always split one, however the threads are scheduled.
fn hammer(
    table: &Mutex<SegmentTable>,
    worker: WorkerId,
    seed: u64,
    start: &Barrier,
) -> Vec<(u64, u64)> {
    let mut rng = XorShift(seed);
    let mut claimed = Vec::new();
    let mut next = table.lock().unwrap().assign(worker);
    start.wait();
    while let Some(assignment) = next {
        let mut pos = assignment.claim;
        loop {
            let n = 1 + rng.next() % (MIB / 2);
            let allowed = {
                let mut t = table.lock().unwrap();
                let allowed = t.claim(assignment.seg_no, worker, pos, n).unwrap();
                t.mark_sent(assignment.seg_no, pos + allowed).unwrap();
                allowed
            };
            if allowed > 0 {
                claimed.push((pos, allowed));
            }
            pos += allowed;
            if allowed < n {
                break;
            }
            if rng.next().is_multiple_of(8) {
                std::thread::yield_now();
            }
        }
        table.lock().unwrap().release(assignment.seg_no, worker);
        next = table.lock().unwrap().assign(worker);
    }
    claimed
}

#[test]
fn stress_shrunk_worker_never_writes_past_end() {
    const THREADS: u64 = 8;
    let total = 384 * MIB + 4321;
    let table = Mutex::new(SegmentTable::plan(total, 4));
    let start = Barrier::new(usize::try_from(THREADS).unwrap());

    let mut claimed: Vec<(u64, u64)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (1..=THREADS)
            .map(|w| {
                let table = &table;
                let start = &start;
                scope.spawn(move || hammer(table, u32::try_from(w).unwrap(), 0x9E37 * w + 1, start))
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });

    let table = table.into_inner().unwrap();
    table.check_invariants().unwrap();
    assert!(table.segments().len() > 4, "the stress run must have split");
    // Every byte was claimed exactly once.
    claimed.sort_unstable();
    let mut next = 0;
    for (pos, len) in &claimed {
        assert_eq!(*pos, next, "gap or overlap at {pos}");
        next = pos + len;
    }
    assert_eq!(next, total);
    // And no claim ever crossed the end its segment finally has: a worker that kept
    // reading after a shrink would straddle a boundary.
    for (pos, len) in &claimed {
        let seg = table
            .segments()
            .iter()
            .find(|s| s.start <= *pos && *pos < s.end)
            .unwrap();
        assert!(
            pos + len <= seg.end,
            "claim {pos}+{len} crosses {}",
            seg.end
        );
    }
    assert!(table.is_complete());
}
