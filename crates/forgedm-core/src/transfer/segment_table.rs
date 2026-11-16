//! The segment table (ARCHITECTURE 4.8, ADR-010): who fetches which byte range, and how far
//! each range has travelled on its way to durable storage.
//!
//! One table exists per segmented download, behind one `std::sync::Mutex` that is never held
//! across an `.await`. Every method here is a short, synchronous critical section, which is
//! what makes claims, splits and checkpoint snapshots atomic with respect to each other.
//!
//! Per segment the offsets obey
//! `start <= committed <= flushed <= written <= sent <= claim <= end`:
//!
//! - `claim`: bytes handed to a worker's buffer (claimed before they are buffered);
//! - `sent`: bytes queued for the writer thread;
//! - `written`: bytes the writer completed (positional writes);
//! - `flushed`: bytes covered by a successful `sync_data`;
//! - `committed`: bytes in a checkpoint that was committed to the database.
//!
//! Segments are disjoint, sorted by `start`, and always tile `[0, total)`; there are at most
//! [`MAX_SEGMENTS`] of them. A split shrinks the victim's `end` and creates the new segment
//! under the same lock, so a worker can never claim a byte that now belongs to another.

use std::time::Duration;

use thiserror::Error;

/// Segments in the initial plan (A.9 `INITIAL_SEGMENTS`).
pub const INITIAL_SEGMENTS: u64 = 4;
/// Most segments a download may have (A.9 `MAX_SEGMENTS`).
pub const MAX_SEGMENTS: usize = 256;
/// Smallest piece a split may leave on either side (A.9 `MIN_SPLIT`).
pub const MIN_SPLIT: u64 = 4 * 1024 * 1024;
/// A segment is only split while at least this much remains (A.9 `SPLIT_MIN_REMAINING`).
pub const SPLIT_MIN_REMAINING: u64 = 8 * 1024 * 1024;
/// Split points are aligned to this (A.9 `SPLIT_ALIGN`).
pub const SPLIT_ALIGN: u64 = 64 * 1024;
/// Initial segment boundaries are aligned to this (A.9 `INITIAL_ALIGN`).
pub const INITIAL_ALIGN: u64 = 1024 * 1024;
/// Delay between the starts of successive connections (A.9 `CONNECTION_STAGGER`).
pub const CONNECTION_STAGGER: Duration = Duration::from_millis(150);
/// Default connections per download (A.9 `CONNS_PER_DOWNLOAD`).
pub const CONNS_PER_DOWNLOAD: u32 = 8;

/// Identifies one worker (connection). Larger ids are younger.
pub type WorkerId = u32;

/// A segment as persisted: `[start, end)` with `committed` bytes durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentSnapshot {
    /// Segment number (renumbered at every checkpoint).
    pub seg_no: u32,
    /// First byte.
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
    /// Next byte to fetch; everything in `[start, committed)` is durable.
    pub committed: u64,
}

/// A table operation that broke the protocol between workers and the table.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TableError {
    /// No segment has this number.
    #[error("no segment {0}")]
    NoSuchSegment(u32),
    /// The caller does not own the segment.
    #[error("segment {0} is not owned by this worker")]
    NotOwner(u32),
    /// A claim did not continue at the segment's claim offset.
    #[error("claim at {got} but segment {seg_no} is at {expected}")]
    PositionMismatch {
        /// Segment number.
        seg_no: u32,
        /// Offset the worker supplied.
        got: u64,
        /// Offset the table expected.
        expected: u64,
    },
    /// The offsets of a segment would stop being ordered.
    #[error("segment {0}: offsets out of order")]
    Order(u32),
    /// A stored segment set is not a valid tiling of the file.
    #[error("invalid segment set: {0}")]
    Invalid(&'static str),
}

/// One contiguous range and its progress. See the module docs for the ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    /// Segment number, unique within the table.
    pub no: u32,
    /// First byte.
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
    /// Next byte to claim.
    pub claim: u64,
    /// Bytes queued for the writer.
    pub sent: u64,
    /// Bytes written by the writer thread.
    pub written: u64,
    /// Bytes made durable by `sync_data`.
    pub flushed: u64,
    /// Bytes committed to the database.
    pub committed: u64,
    /// The worker fetching this range, if any.
    pub owner: Option<WorkerId>,
    /// Smoothed speed of the owner in bytes per second (split decisions).
    pub speed_bps: f64,
}

impl Seg {
    fn fresh(no: u32, start: u64, end: u64, at: u64) -> Self {
        Self {
            no,
            start,
            end,
            claim: at,
            sent: at,
            written: at,
            flushed: at,
            committed: at,
            owner: None,
            speed_bps: 0.0,
        }
    }

    fn remaining(&self) -> u64 {
        self.end - self.claim
    }
}

/// What a worker is told to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assignment {
    /// The segment.
    pub seg_no: u32,
    /// First byte to request.
    pub claim: u64,
    /// One past the last byte at assignment time (it may shrink later).
    pub end: u64,
}

/// A consistent view for one checkpoint: the records to persist (adjacent fully committed
/// segments merged) and the per-segment offsets to publish once the commit succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointView {
    /// What to write to the database.
    pub records: Vec<SegmentSnapshot>,
    /// `(segment number, flushed offset)` of every live segment.
    pub flushed: Vec<(u32, u64)>,
}

/// The segment table of one download.
#[derive(Debug)]
pub struct SegmentTable {
    total: u64,
    segs: Vec<Seg>,
    next_no: u32,
    base: u64,
    sent_total: u64,
}

fn align_down(value: u64, align: u64) -> u64 {
    value - value % align
}

impl SegmentTable {
    /// The initial plan: `min(4, max_connections, ceil(total / MIN_SPLIT))` equal segments
    /// with boundaries aligned to 1 MiB, or one segment when `total < 2 * MIN_SPLIT`.
    #[must_use]
    pub fn plan(total: u64, max_connections: u32) -> Self {
        let mut bounds = vec![0_u64];
        if total >= 2 * MIN_SPLIT {
            let count = INITIAL_SEGMENTS
                .min(u64::from(max_connections))
                .min(total.div_ceil(MIN_SPLIT))
                .max(1);
            for i in 1..count {
                let raw = u128::from(total) * u128::from(i) / u128::from(count);
                let boundary = align_down(u64::try_from(raw).unwrap_or(total), INITIAL_ALIGN);
                if bounds.last().is_some_and(|last| boundary > *last) && boundary < total {
                    bounds.push(boundary);
                }
            }
        }
        let mut segs = Vec::new();
        if total > 0 {
            bounds.push(total);
            for (no, pair) in (0_u32..).zip(bounds.windows(2)) {
                segs.push(Seg::fresh(no, pair[0], pair[1], pair[0]));
            }
        }
        let next_no = u32::try_from(segs.len()).unwrap_or(u32::MAX);
        Self {
            total,
            segs,
            next_no,
            base: 0,
            sent_total: 0,
        }
    }

    /// Restores a table from persisted segments: every segment continues at its committed
    /// offset.
    ///
    /// # Errors
    /// [`TableError::Invalid`] unless the segments tile `[0, total)` with
    /// `start <= committed <= end` (at most [`MAX_SEGMENTS`]).
    pub fn from_snapshots(snapshots: &[SegmentSnapshot], total: u64) -> Result<Self, TableError> {
        if snapshots.len() > MAX_SEGMENTS {
            return Err(TableError::Invalid("too many segments"));
        }
        let mut sorted: Vec<&SegmentSnapshot> = snapshots.iter().collect();
        sorted.sort_by_key(|s| s.start);
        let mut next = 0_u64;
        let mut segs = Vec::with_capacity(sorted.len());
        for (no, snap) in (0_u32..).zip(sorted) {
            if snap.start != next || snap.end <= snap.start {
                return Err(TableError::Invalid("gap, overlap or empty segment"));
            }
            if snap.committed < snap.start || snap.committed > snap.end {
                return Err(TableError::Invalid("committed outside its segment"));
            }
            segs.push(Seg::fresh(no, snap.start, snap.end, snap.committed));
            next = snap.end;
        }
        if next != total {
            return Err(TableError::Invalid("segments do not cover the total"));
        }
        let next_no = u32::try_from(segs.len()).unwrap_or(u32::MAX);
        let mut table = Self {
            total,
            segs,
            next_no,
            base: 0,
            sent_total: 0,
        };
        table.base = table.committed_bytes();
        Ok(table)
    }

    /// The complete length the segments tile.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// The segments, ordered by `start`.
    #[must_use]
    pub fn segments(&self) -> &[Seg] {
        &self.segs
    }

    /// The segment numbered `no`.
    #[must_use]
    pub fn segment(&self, no: u32) -> Option<&Seg> {
        self.segs.iter().find(|s| s.no == no)
    }

    /// Bytes committed across segments: the sum of `committed - start`.
    #[must_use]
    pub fn committed_bytes(&self) -> u64 {
        self.segs.iter().map(|s| s.committed - s.start).sum()
    }

    /// Bytes accepted by this table: what was committed when it was built plus everything
    /// queued for the writer since. Never decreases.
    #[must_use]
    pub const fn accepted_bytes(&self) -> u64 {
        self.base + self.sent_total
    }

    /// Whether every byte has been claimed (nothing is left to request).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.segs.iter().all(|s| s.claim == s.end)
    }

    /// Whether every byte has been written by the writer thread.
    #[must_use]
    pub fn is_fully_written(&self) -> bool {
        self.segs.iter().all(|s| s.written == s.end)
    }

    /// The largest `end` any segment has committed up to, for the staging file length check.
    #[must_use]
    pub fn max_committed(&self) -> u64 {
        self.segs.iter().map(|s| s.committed).max().unwrap_or(0)
    }

    /// The committed offset of the segment with the most committed data beyond its own start,
    /// when that is at least `window` bytes: where a weak-validator sample takes its window.
    /// `None` when no segment has `window` committed bytes (nothing can be sampled).
    #[must_use]
    pub fn sample_point(&self, window: u64) -> Option<u64> {
        self.segs
            .iter()
            .filter(|s| s.committed - s.start >= window)
            .max_by_key(|s| s.committed - s.start)
            .map(|s| s.committed)
    }

    fn find(&self, no: u32) -> Result<usize, TableError> {
        self.segs
            .iter()
            .position(|s| s.no == no)
            .ok_or(TableError::NoSuchSegment(no))
    }

    fn owned(&mut self, no: u32, worker: WorkerId) -> Result<&mut Seg, TableError> {
        let index = self.find(no)?;
        let seg = &mut self.segs[index];
        if seg.owner == Some(worker) {
            Ok(seg)
        } else {
            Err(TableError::NotOwner(no))
        }
    }

    /// Gives `worker` work: the unowned segment with work left and the lowest start, else
    /// half of the owned segment that would take longest to finish (see
    /// [`Self::has_assignable`]). `None` means nothing can be given.
    pub fn assign(&mut self, worker: WorkerId) -> Option<Assignment> {
        if let Some(seg) = self
            .segs
            .iter_mut()
            .find(|s| s.owner.is_none() && s.claim < s.end)
        {
            seg.owner = Some(worker);
            return Some(Assignment {
                seg_no: seg.no,
                claim: seg.claim,
                end: seg.end,
            });
        }
        let (victim, mid) = self.best_split()?;
        let no = self.next_no;
        self.next_no = self.next_no.checked_add(1)?;
        let end = self.segs[victim].end;
        self.segs[victim].end = mid;
        let mut fresh = Seg::fresh(no, mid, end, mid);
        fresh.owner = Some(worker);
        self.segs.insert(victim + 1, fresh);
        crate::fault::hit(crate::fault::FaultPoint::MidSplit);
        Some(Assignment {
            seg_no: no,
            claim: mid,
            end,
        })
    }

    /// Whether [`Self::assign`] would find work right now.
    #[must_use]
    pub fn has_assignable(&self) -> bool {
        self.segs
            .iter()
            .any(|s| s.owner.is_none() && s.claim < s.end)
            || self.best_split().is_some()
    }

    /// The split victim and the split point: the owned segment with the largest
    /// `remaining / speed` among those with at least [`SPLIT_MIN_REMAINING`] left whose
    /// aligned midpoint leaves [`MIN_SPLIT`] on both sides.
    fn best_split(&self) -> Option<(usize, u64)> {
        if self.segs.len() >= MAX_SEGMENTS {
            return None;
        }
        self.segs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.owner.is_some() && s.remaining() >= SPLIT_MIN_REMAINING)
            .filter_map(|(index, s)| {
                let mid = (s.claim + s.remaining() / 2).div_ceil(SPLIT_ALIGN) * SPLIT_ALIGN;
                (mid - s.claim >= MIN_SPLIT && s.end - mid >= MIN_SPLIT).then(|| {
                    let eta = s.remaining() as f64 / s.speed_bps.max(1.0);
                    (index, mid, eta)
                })
            })
            .max_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(index, mid, _)| (index, mid))
    }

    /// Claims the next `n` bytes at stream position `pos` of segment `no` for `worker` and
    /// returns how many were allowed: `min(n, end - claim)`. Less than `n` means the
    /// segment shrank or ended; the worker flushes and stops.
    ///
    /// # Errors
    /// [`TableError::NotOwner`] or [`TableError::PositionMismatch`], which abort the worker
    /// as a bug.
    pub fn claim(
        &mut self,
        no: u32,
        worker: WorkerId,
        pos: u64,
        n: u64,
    ) -> Result<u64, TableError> {
        let seg = self.owned(no, worker)?;
        if pos != seg.claim {
            return Err(TableError::PositionMismatch {
                seg_no: no,
                got: pos,
                expected: seg.claim,
            });
        }
        let allowed = n.min(seg.end - seg.claim);
        seg.claim += allowed;
        Ok(allowed)
    }

    /// Records that bytes up to `upto` of segment `no` are queued for the writer. Returns
    /// the table's accepted byte count.
    ///
    /// # Errors
    /// [`TableError::Order`] unless `sent <= upto <= claim`.
    pub fn mark_sent(&mut self, no: u32, upto: u64) -> Result<u64, TableError> {
        let index = self.find(no)?;
        let seg = &mut self.segs[index];
        if upto < seg.sent || upto > seg.claim {
            return Err(TableError::Order(no));
        }
        self.sent_total += upto - seg.sent;
        seg.sent = upto;
        Ok(self.accepted_bytes())
    }

    /// Gives a segment back: bytes claimed but never queued return to the unclaimed range
    /// (`claim = sent`), and the segment becomes unowned. A no-op for a non-owner.
    pub fn release(&mut self, no: u32, worker: WorkerId) {
        if let Ok(seg) = self.owned(no, worker) {
            seg.claim = seg.sent;
            seg.owner = None;
        }
    }

    /// Records the smoothed speed of segment `no`'s owner.
    pub fn set_speed(&mut self, no: u32, bytes_per_second: f64) {
        if let Ok(index) = self.find(no) {
            self.segs[index].speed_bps = bytes_per_second;
        }
    }

    /// The writer completed a positional write of `[offset, offset + len)`.
    pub fn note_written(&mut self, offset: u64, len: u64) {
        let after = self.segs.partition_point(|s| s.start <= offset);
        let Some(seg) = after.checked_sub(1).map(|i| &mut self.segs[i]) else {
            return;
        };
        let upto = (offset + len).min(seg.sent);
        debug_assert!(offset + len <= seg.sent, "write beyond what was sent");
        seg.written = seg.written.max(upto);
    }

    /// `(segment, written)` for every segment: the frontier a `sync_data` is about to make
    /// durable (taken before the sync, so it is never an overestimate).
    #[must_use]
    pub fn written_frontier(&self) -> Vec<(u32, u64)> {
        self.segs.iter().map(|s| (s.no, s.written)).collect()
    }

    /// A `sync_data` that followed [`Self::written_frontier`] succeeded.
    pub fn note_flushed(&mut self, frontier: &[(u32, u64)]) {
        for &(no, written) in frontier {
            if let Ok(index) = self.find(no) {
                let seg = &mut self.segs[index];
                seg.flushed = seg.flushed.max(written.min(seg.written));
            }
        }
    }

    /// The records a checkpoint persists: each segment's flushed offset as its committed
    /// offset, with adjacent fully committed segments merged and numbers reassigned.
    #[must_use]
    pub fn checkpoint_view(&self) -> CheckpointView {
        let mut records: Vec<SegmentSnapshot> = Vec::with_capacity(self.segs.len());
        let mut previous_done = false;
        for seg in &self.segs {
            let done = seg.flushed == seg.end;
            match records.last_mut() {
                Some(last) if done && previous_done => {
                    last.end = seg.end;
                    last.committed = seg.end;
                }
                _ => records.push(SegmentSnapshot {
                    seg_no: u32::try_from(records.len()).unwrap_or(u32::MAX),
                    start: seg.start,
                    end: seg.end,
                    committed: seg.flushed,
                }),
            }
            previous_done = done;
        }
        CheckpointView {
            records,
            flushed: self.segs.iter().map(|s| (s.no, s.flushed)).collect(),
        }
    }

    /// The checkpoint of `view` was committed: publishes it as `committed` and returns the
    /// committed byte count.
    pub fn apply_committed(&mut self, view: &CheckpointView) -> u64 {
        for &(no, flushed) in &view.flushed {
            if let Ok(index) = self.find(no) {
                let seg = &mut self.segs[index];
                seg.committed = seg.committed.max(flushed.min(seg.flushed));
            }
        }
        self.committed_bytes()
    }

    /// Checks every invariant of the module docs plus the tiling.
    ///
    /// # Errors
    /// A description of the first violated invariant.
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        if self.segs.len() > MAX_SEGMENTS {
            return Err("more than MAX_SEGMENTS segments");
        }
        let mut next = 0_u64;
        for seg in &self.segs {
            if seg.start != next {
                return Err("gap or overlap between segments");
            }
            let ordered = seg.start <= seg.committed
                && seg.committed <= seg.flushed
                && seg.flushed <= seg.written
                && seg.written <= seg.sent
                && seg.sent <= seg.claim
                && seg.claim <= seg.end;
            if !ordered || seg.end <= seg.start {
                return Err("segment offsets out of order");
            }
            next = seg.end;
        }
        if next != self.total {
            return Err("segments do not cover the total");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn ranges(table: &SegmentTable) -> Vec<(u64, u64)> {
        table.segments().iter().map(|s| (s.start, s.end)).collect()
    }

    #[test]
    fn the_sample_point_is_the_fullest_segment_with_a_whole_window() {
        let snap = |no, start, end, committed| SegmentSnapshot {
            seg_no: no,
            start,
            end,
            committed,
        };
        let table = SegmentTable::from_snapshots(
            &[
                snap(0, 0, 1000, 600),
                snap(1, 1000, 2000, 1900),
                snap(2, 2000, 3000, 2000),
            ],
            3000,
        )
        .unwrap();

        assert_eq!(table.sample_point(500), Some(1900));
        assert_eq!(table.sample_point(901), None, "no segment has that much");
        assert_eq!(table.sample_point(900), Some(1900));
    }

    #[test]
    fn small_files_get_one_segment_and_large_files_four() {
        assert_eq!(
            ranges(&SegmentTable::plan(8 * MIB - 1, 8)),
            [(0, 8 * MIB - 1)]
        );
        assert_eq!(
            ranges(&SegmentTable::plan(64 * MIB, 8)),
            [
                (0, 16 * MIB),
                (16 * MIB, 32 * MIB),
                (32 * MIB, 48 * MIB),
                (48 * MIB, 64 * MIB)
            ]
        );
        assert_eq!(SegmentTable::plan(64 * MIB, 1).segments().len(), 1);
        assert_eq!(SegmentTable::plan(64 * MIB, 3).segments().len(), 3);
        assert_eq!(SegmentTable::plan(8 * MIB, 8).segments().len(), 2);
    }

    #[test]
    fn initial_boundaries_are_aligned_and_cover_the_file() {
        let total = 64 * MIB + 12_345;
        let table = SegmentTable::plan(total, 4);

        table.check_invariants().unwrap();
        for seg in &table.segments()[1..] {
            assert_eq!(seg.start % INITIAL_ALIGN, 0);
        }
        assert_eq!(table.segments().last().unwrap().end, total);
    }

    #[test]
    fn a_split_halves_the_victim_at_an_aligned_midpoint() {
        let mut table = SegmentTable::plan(32 * MIB, 1);
        let first = table.assign(1).unwrap();
        assert_eq!((first.claim, first.end), (0, 32 * MIB));

        let second = table.assign(2).unwrap();

        assert_eq!(second.claim, 16 * MIB);
        assert_eq!(second.end, 32 * MIB);
        assert_eq!(table.segments()[0].end, 16 * MIB, "the victim shrank");
        table.check_invariants().unwrap();
    }

    #[test]
    fn nothing_is_split_below_the_remaining_threshold() {
        let mut table = SegmentTable::plan(8 * MIB, 1);
        table.assign(1).unwrap();
        assert!(table.claim(0, 1, 0, MIB).is_ok());
        assert!(!table.has_assignable(), "7 MiB left is below 8 MiB");
        assert_eq!(table.assign(2), None);
    }

    #[test]
    fn the_victim_is_the_segment_with_the_most_time_left() {
        let mut table = SegmentTable::plan(64 * MIB, 4);
        for worker in 1..=4 {
            table.assign(worker).unwrap();
        }
        table.set_speed(0, 8.0 * MIB as f64);
        table.set_speed(1, 8.0 * MIB as f64);
        table.set_speed(2, 0.5 * MIB as f64);
        table.set_speed(3, 8.0 * MIB as f64);

        let stolen = table.assign(5).unwrap();

        assert!(
            (32 * MIB..48 * MIB).contains(&stolen.claim),
            "stole from the slow segment 2, got {stolen:?}"
        );
    }

    #[test]
    fn unowned_work_is_taken_before_anything_is_split() {
        let mut table = SegmentTable::plan(64 * MIB, 4);
        for worker in 1..=4 {
            table.assign(worker).unwrap();
        }
        table.release(2, 3);

        let next = table.assign(9).unwrap();

        assert_eq!(next.seg_no, 2);
        assert_eq!(table.segments().len(), 4, "no split happened");
    }

    #[test]
    fn claims_must_continue_at_the_claim_offset_and_stop_at_the_end() {
        let mut table = SegmentTable::plan(MIB, 1);
        table.assign(1).unwrap();

        assert_eq!(table.claim(0, 1, 0, 1000).unwrap(), 1000);
        assert_eq!(
            table.claim(0, 1, 5, 10),
            Err(TableError::PositionMismatch {
                seg_no: 0,
                got: 5,
                expected: 1000
            })
        );
        assert_eq!(table.claim(0, 2, 1000, 1), Err(TableError::NotOwner(0)));
        assert_eq!(table.claim(0, 1, 1000, 5 * MIB).unwrap(), MIB - 1000);
        assert_eq!(table.claim(0, 1, MIB, 1).unwrap(), 0);
        assert!(table.is_complete());
    }

    #[test]
    fn release_returns_unsent_bytes_to_the_unclaimed_range() {
        let mut table = SegmentTable::plan(MIB, 1);
        table.assign(1).unwrap();
        table.claim(0, 1, 0, 500).unwrap();
        table.mark_sent(0, 300).unwrap();

        table.release(0, 1);

        let seg = &table.segments()[0];
        assert_eq!((seg.claim, seg.sent, seg.owner), (300, 300, None));
        assert_eq!(table.assign(2).unwrap().claim, 300);
    }

    #[test]
    fn mark_sent_cannot_pass_the_claim_or_go_back() {
        let mut table = SegmentTable::plan(MIB, 1);
        table.assign(1).unwrap();
        table.claim(0, 1, 0, 100).unwrap();

        assert_eq!(table.mark_sent(0, 101), Err(TableError::Order(0)));
        assert_eq!(table.mark_sent(0, 60).unwrap(), 60);
        assert_eq!(table.mark_sent(0, 50), Err(TableError::Order(0)));
        assert_eq!(table.accepted_bytes(), 60);
    }

    #[test]
    fn writes_flushes_and_commits_advance_in_order() {
        let mut table = SegmentTable::plan(MIB, 1);
        table.assign(1).unwrap();
        table.claim(0, 1, 0, 1000).unwrap();
        table.mark_sent(0, 1000).unwrap();
        table.note_written(0, 600);
        let frontier = table.written_frontier();
        table.note_written(600, 400);
        table.note_flushed(&frontier);

        let view = table.checkpoint_view();
        assert_eq!(view.records[0].committed, 600, "only what was flushed");
        assert_eq!(table.apply_committed(&view), 600);
        table.check_invariants().unwrap();
    }

    #[test]
    fn checkpoint_records_merge_adjacent_complete_segments() {
        let mut table = SegmentTable::plan(32 * MIB, 4);
        for no in 0..4 {
            let seg = table.segments()[no as usize].clone();
            table.assign(no + 1).unwrap();
            table
                .claim(seg.no, no + 1, seg.start, seg.end - seg.start)
                .unwrap();
            table.mark_sent(seg.no, seg.end).unwrap();
            if no != 2 {
                table.note_written(seg.start, seg.end - seg.start);
            }
        }
        let frontier = table.written_frontier();
        table.note_flushed(&frontier);

        let view = table.checkpoint_view();

        assert_eq!(
            view.records.len(),
            3,
            "segments 0 and 1 merged: {:?}",
            view.records
        );
        assert_eq!((view.records[0].start, view.records[0].end), (0, 16 * MIB));
        assert_eq!(view.records[1].committed, view.records[1].start);
    }

    #[test]
    fn restored_tables_continue_at_committed_offsets() {
        let snaps = [
            SegmentSnapshot {
                seg_no: 7,
                start: 0,
                end: 10,
                committed: 4,
            },
            SegmentSnapshot {
                seg_no: 9,
                start: 10,
                end: 20,
                committed: 20,
            },
        ];

        let table = SegmentTable::from_snapshots(&snaps, 20).unwrap();

        assert_eq!(table.committed_bytes(), 14);
        assert_eq!(table.accepted_bytes(), 14);
        assert_eq!(table.segments()[0].claim, 4);
        table.check_invariants().unwrap();
    }

    #[test]
    fn invalid_stored_segments_are_refused() {
        let gap = [SegmentSnapshot {
            seg_no: 0,
            start: 1,
            end: 20,
            committed: 1,
        }];
        let beyond = [SegmentSnapshot {
            seg_no: 0,
            start: 0,
            end: 20,
            committed: 21,
        }];
        let short = [SegmentSnapshot {
            seg_no: 0,
            start: 0,
            end: 10,
            committed: 0,
        }];
        for bad in [&gap[..], &beyond[..], &short[..]] {
            assert!(matches!(
                SegmentTable::from_snapshots(bad, 20),
                Err(TableError::Invalid(_))
            ));
        }
    }
}
