//! Checkpointing: the order that makes a committed byte count trustworthy (PLAN S1.9,
//! ARCHITECTURE 5.4).
//!
//! 1. the writer thread flushes everything handed to it and runs `sync_data`;
//! 2. the writer answers with the frontier it made durable;
//! 3. the store commits that frontier in one `BEGIN IMMEDIATE` transaction;
//! 4. only then is the number published to progress readers.
//!
//! A commit therefore never exceeds the last flush, a failed `sync_data` stops the sequence
//! before the commit, and nothing is published that is not committed.

use std::sync::Arc;

use uuid::Uuid;

use crate::clock::BoxFuture;
use crate::engine::LiveDownload;
use crate::error::{TaskStop, stop_from_store};
use crate::ports::DownloadStore;
use crate::transfer::single::{Checkpointer, writer_stop};
use crate::writer::Writer;

/// Flushes `writer` (including `sync_data`), commits the flushed frontier through
/// `committer`, then publishes it on `live`. Returns the committed frontier.
///
/// # Errors
/// A writer failure (including a failed `sync_data`, which prevents the commit) or the
/// committer's failure (nothing is published).
pub async fn flush_then_commit(
    writer: &mut Writer,
    committer: &dyn Checkpointer,
    live: &LiveDownload,
) -> Result<u64, TaskStop> {
    let frontier = writer.flush().await.map_err(|e| writer_stop(&e))?;
    committer.checkpoint(frontier).await?;
    live.set_committed(frontier);
    Ok(frontier)
}

/// Commits checkpoints to the [`DownloadStore`].
pub struct StoreCommitter {
    store: Arc<dyn DownloadStore>,
    id: Uuid,
    total: Option<u64>,
}

impl StoreCommitter {
    /// A committer for download `id` whose segment ends at `total` (open-ended when
    /// unknown).
    #[must_use]
    pub fn new(store: Arc<dyn DownloadStore>, id: Uuid, total: Option<u64>) -> Self {
        Self { store, id, total }
    }
}

impl Checkpointer for StoreCommitter {
    fn checkpoint(&self, committed: u64) -> BoxFuture<'_, Result<(), TaskStop>> {
        Box::pin(async move {
            self.store
                .checkpoint(self.id, committed, self.total)
                .await
                .map_err(|e| stop_from_store(&e))
        })
    }
}
