//! The checkpoint order (PLAN S1.9): writer `Flush` (which runs `sync_data`), then the
//! commit, then publish. A commit never exceeds what the last flush made durable, and a
//! failed `sync_data` blocks the commit.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::io;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};

use forgedm_core::checkpoint::flush_then_commit;
use forgedm_core::clock::BoxFuture;
use forgedm_core::engine::{LiveDownload, StopSignal};
use forgedm_core::error::TaskStop;
use forgedm_core::transfer::single::Checkpointer;
use forgedm_core::writer::{Sink, Writer};
use uuid::Uuid;

type Ops = Arc<Mutex<Vec<String>>>;

fn push(ops: &Ops, op: String) {
    ops.lock().unwrap_or_else(PoisonError::into_inner).push(op);
}

/// Records every operation in one shared, ordered list.
struct RecordingSink {
    ops: Ops,
    fail_sync: bool,
}

impl Sink for RecordingSink {
    fn write_at(&mut self, buf: &[u8], offset: u64) -> io::Result<usize> {
        push(&self.ops, format!("write {offset} {}", buf.len()));
        Ok(buf.len())
    }

    fn sync_data(&mut self) -> io::Result<()> {
        push(&self.ops, "sync_data".to_owned());
        if self.fail_sync {
            Err(io::Error::other("injected sync_data failure"))
        } else {
            Ok(())
        }
    }

    fn sync_all(&mut self) -> io::Result<()> {
        push(&self.ops, "sync_all".to_owned());
        Ok(())
    }
}

struct RecordingCommitter {
    ops: Ops,
}

impl Checkpointer for RecordingCommitter {
    fn checkpoint(&self, committed: u64) -> BoxFuture<'_, Result<(), TaskStop>> {
        push(&self.ops, format!("commit {committed}"));
        Box::pin(async { Ok(()) })
    }
}

struct Rig {
    ops: Ops,
    writer: Writer,
    committer: RecordingCommitter,
    live: LiveDownload,
}

fn rig(fail_sync: bool) -> Rig {
    let ops: Ops = Arc::default();
    let sink = RecordingSink {
        ops: Arc::clone(&ops),
        fail_sync,
    };
    let writer = Writer::spawn(
        "checkpoint-test".to_owned(),
        sink,
        Arc::new(AtomicU64::new(0)),
    )
    .expect("writer thread");
    Rig {
        committer: RecordingCommitter {
            ops: Arc::clone(&ops),
        },
        ops,
        writer,
        live: LiveDownload::new(Uuid::nil(), StopSignal::new()),
    }
}

fn recorded(ops: &Ops) -> Vec<String> {
    ops.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

#[tokio::test]
async fn commit_never_exceeds_last_flush() {
    let mut r = rig(false);
    r.writer.write(0, &vec![1u8; 300_000]).await.unwrap();
    r.live.set_accepted(300_000);
    let first = flush_then_commit(&mut r.writer, &r.committer, &r.live)
        .await
        .unwrap();
    // These bytes are accepted after the flush: the next commit must not include them
    // unless it flushes them first.
    r.writer.write(300_000, &[2u8; 1_000]).await.unwrap();
    r.live.set_accepted(301_000);
    let second = flush_then_commit(&mut r.writer, &r.committer, &r.live)
        .await
        .unwrap();

    assert_eq!((first, second), (300_000, 301_000));
    let ops = recorded(&r.ops);
    let mut written_at_last_sync = 0u64;
    let mut written = 0u64;
    let mut commits = 0;
    for (i, op) in ops.iter().enumerate() {
        let parts: Vec<&str> = op.split(' ').collect();
        match parts.as_slice() {
            ["write", _, len] => written += len.parse::<u64>().unwrap(),
            ["sync_data"] => written_at_last_sync = written,
            ["commit", n] => {
                commits += 1;
                let n: u64 = n.parse().unwrap();
                assert_eq!(
                    ops[i - 1],
                    "sync_data",
                    "a flush directly precedes: {ops:?}"
                );
                assert!(
                    n <= written_at_last_sync,
                    "commit {n} > flushed {written_at_last_sync}"
                );
                assert_eq!(
                    n, written_at_last_sync,
                    "and it commits exactly the flushed bytes"
                );
            }
            other => panic!("unexpected op {other:?}"),
        }
    }
    assert_eq!(commits, 2);
    assert_eq!(
        r.live.peek().committed,
        301_000,
        "published after the commit"
    );
}

#[tokio::test]
async fn sync_data_failure_blocks_commit() {
    let mut r = rig(true);
    r.writer.write(0, &[7u8; 5_000]).await.unwrap();
    r.live.set_accepted(5_000);

    let result = flush_then_commit(&mut r.writer, &r.committer, &r.live).await;

    assert!(result.is_err(), "a failed sync_data fails the checkpoint");
    let ops = recorded(&r.ops);
    assert!(
        ops.iter().all(|op| !op.starts_with("commit")),
        "nothing was committed: {ops:?}"
    );
    assert_eq!(r.live.peek().committed, 0, "nothing was published");
}
