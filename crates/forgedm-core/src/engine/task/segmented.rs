//! The segmented transfer inside a download task (PLAN S1.11): staging, restoring the segment
//! table after a pause or a crash, and handing everything to [`SegmentedTransfer`].

use std::fs::File;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use forgedm_protocol::TransferMode;
use url::Url;

use super::{DownloadTask, Landed, internal};
use crate::error::{TaskStop, stop_from_io, stop_from_store};
use crate::http::validators::ResumeIdentity;
use crate::probe::Probed;
use crate::resume::Continuation;
use crate::state::Command;
use crate::transfer::prealloc::clear_sparse;
use crate::transfer::segment_table::SegmentTable;
use crate::transfer::segmented::{SegmentedTransfer, TableTracker};
use crate::transfer::single::{CHECKPOINT_INTERVAL, Plan, choose_name, id8, writer_stop};
use crate::writer::Writer;

impl DownloadTask {
    /// A fresh download whose probe verified a 206 with a known `total` of 8 MiB or more:
    /// stage the (sparse) file, persist the initial plan, then run the segmented transfer.
    pub(super) async fn fresh_segmented(
        &mut self,
        probed: Probed,
        total: u64,
    ) -> Result<Landed, TaskStop> {
        let name = choose_name(
            &self.job,
            probed.content_disposition.as_deref(),
            &probed.final_url,
        );
        let plan = Plan {
            transfer_mode: TransferMode::Segmented,
            total: Some(total),
            mime: probed.mime.clone(),
            validators: probed.result.validators.clone(),
        };
        let staged = self.stage(&plan, &probed.final_url, &name).await?;
        let path = staged.path.clone();
        let identity = ResumeIdentity::from_validators(Some(total), &plan.validators);
        let table = SegmentTable::plan(total, self.ctx.cap.get());
        self.ctx
            .store
            .checkpoint_segments(self.job.id, table.checkpoint_view().records)
            .await
            .map_err(|e| stop_from_store(&e))?;
        let final_url = self
            .segmented_transfer(staged.file, table, identity)
            .await?;
        Ok(Landed {
            path,
            final_url,
            name,
            total: Some(total),
        })
    }

    /// The segment table a resumed segmented download continues from: the persisted segments,
    /// or a fresh plan when nothing was ever checkpointed. `None` when the persisted
    /// segments cannot be trusted (the caller restarts from zero).
    pub(super) async fn restore_table(
        &self,
        from: &Continuation,
    ) -> Result<Option<SegmentTable>, TaskStop> {
        let snapshots = self
            .ctx
            .store
            .load_segments(self.job.id)
            .await
            .map_err(|e| stop_from_store(&e))?;
        if snapshots.is_empty() && from.committed == 0 {
            return Ok(Some(SegmentTable::plan(from.total, self.ctx.cap.get())));
        }
        match SegmentTable::from_snapshots(&snapshots, from.total) {
            Ok(table) => Ok(Some(table)),
            Err(err) => {
                tracing::warn!(error = %err, "stored segments are not valid; restarting from zero");
                Ok(None)
            }
        }
    }

    /// Runs `table` over `file` and moves the download to `verifying`. Returns the URL of
    /// the first accepted response.
    pub(super) async fn segmented_transfer(
        &mut self,
        file: File,
        table: SegmentTable,
        identity: ResumeIdentity,
    ) -> Result<Url, TaskStop> {
        let id = self.job.id;
        let sparse_handle = file.try_clone().map_err(|e| stop_from_io(&e))?;
        let table = Arc::new(Mutex::new(table));
        let tracker = Arc::new(TableTracker(Arc::clone(&table)));
        let name = format!("forgedm-writer-{}", id8(id, 0));
        let written = Arc::new(AtomicU64::new(0));
        let writer =
            Writer::spawn_tracked(name, file, written, tracker).map_err(|e| writer_stop(&e))?;
        let transfer = SegmentedTransfer {
            id,
            store: Arc::clone(&self.ctx.store),
            live: Arc::clone(&self.ctx.live),
            client: self.ctx.client.clone(),
            url: self.job.url.as_url().clone(),
            identity,
            dir: self.job.target_dir.clone(),
            cap: Arc::clone(&self.ctx.cap),
            checkpoint_every: CHECKPOINT_INTERVAL,
        };
        let final_url = transfer.run(table, writer).await?;
        clear_sparse(&sparse_handle);
        drop(sparse_handle);
        self.advance(Command::TransferComplete, None).await?;
        final_url
            .or_else(|| Some(self.job.url.as_url().clone()))
            .ok_or_else(internal)
    }
}
