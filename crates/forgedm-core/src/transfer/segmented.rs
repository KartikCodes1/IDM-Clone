//! The segmented transfer (PLAN S1.11, ARCHITECTURE 4.8, ADR-010): up to N connections, each
//! fetching a disjoint range of one file, stealing work from the slowest range when they run
//! out, all writing through one writer thread.
//!
//! [`SegmentedTransfer::run`] is the supervisor. It starts workers (staggered, up to the
//! connection cap), starts more when the cap rises and work exists, retires the youngest when
//! it falls (their ranges become unowned and the others pick them up), checkpoints the whole
//! segment table every [`CHECKPOINT_INTERVAL`], and on any stop or failure halts every worker,
//! checkpoints what is durable and closes the writer.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use forgedm_protocol::DownloadErrorCode;
use forgedm_protocol::{MAX_CONNECTIONS, MIN_CONNECTIONS};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use super::segment_table::{CONNECTION_STAGGER, SegmentTable};
use super::single::{FREE_SPACE_RECHECK, ensure_free_space, writer_stop};
use super::worker::{Shared, SharedTable, WorkerEnd, run_worker};
use crate::checkpoint::{commit_segments, flush_then_commit_segments};
use crate::client::HttpClient;
use crate::engine::LiveDownload;
use crate::error::{Failure, TaskStop};
use crate::http::validators::ResumeIdentity;
use crate::ports::DownloadStore;
use crate::writer::{Frontier, WriteTracker, Writer};

/// How often the supervisor looks for work for idle connection slots.
const REBALANCE_TICK: Duration = Duration::from_millis(250);

/// The live connection cap of one download; changing it takes effect within a tick.
#[derive(Debug)]
pub struct ConnectionCap {
    tx: watch::Sender<u32>,
}

impl ConnectionCap {
    /// A cap of `value`, clamped to `1..=16`.
    #[must_use]
    pub fn new(value: u32) -> Self {
        Self {
            tx: watch::channel(value.clamp(MIN_CONNECTIONS, MAX_CONNECTIONS)).0,
        }
    }

    /// The current cap.
    #[must_use]
    pub fn get(&self) -> u32 {
        *self.tx.borrow()
    }

    /// Changes the cap (clamped to `1..=16`) and wakes the supervisor.
    pub fn set(&self, value: u32) {
        self.tx
            .send_replace(value.clamp(MIN_CONNECTIONS, MAX_CONNECTIONS));
    }

    fn subscribe(&self) -> watch::Receiver<u32> {
        self.tx.subscribe()
    }
}

/// The caps of every known download, by id. Entries are tiny and bounded by the number of known downloads; one is dropped when its download is removed. Live changes last for the agent session only (they are not stored).
#[derive(Debug, Default)]
pub struct ConnectionCaps {
    map: Mutex<HashMap<Uuid, Arc<ConnectionCap>>>,
}

impl ConnectionCaps {
    /// The cap of `id`, created with `default` when there is none yet.
    pub fn cap_for(&self, id: Uuid, default: u32) -> Arc<ConnectionCap> {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            map.entry(id)
                .or_insert_with(|| Arc::new(ConnectionCap::new(default))),
        )
    }

    /// Sets the cap of `id` (creating it when needed).
    pub fn set(&self, id: Uuid, value: u32) {
        self.cap_for(id, value).set(value);
    }

    /// Forgets `id` (the download was removed).
    pub fn forget(&self, id: Uuid) {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }
}

/// Lets the writer thread keep the segment table's `written` and `flushed` offsets current.
pub struct TableTracker(pub SharedTable);

impl TableTracker {
    fn table(&self) -> std::sync::MutexGuard<'_, SegmentTable> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl WriteTracker for TableTracker {
    fn wrote(&self, offset: u64, len: u64) {
        self.table().note_written(offset, len);
    }

    fn before_sync(&self) -> Frontier {
        self.table().written_frontier()
    }

    fn synced(&self, frontier: &Frontier) {
        self.table().note_flushed(frontier);
    }
}

/// Everything a segmented transfer needs besides the table and the writer.
pub struct SegmentedTransfer {
    /// Download id.
    pub id: Uuid,
    /// Persistence of checkpoints.
    pub store: Arc<dyn DownloadStore>,
    /// Counters and the stop signal.
    pub live: Arc<LiveDownload>,
    /// The HTTP client.
    pub client: HttpClient,
    /// The original URL.
    pub url: Url,
    /// The stored remote identity.
    pub identity: ResumeIdentity,
    /// Directory of the staging file (free-space re-checks).
    pub dir: PathBuf,
    /// The live connection cap.
    pub cap: Arc<ConnectionCap>,
    /// Time between checkpoints.
    pub checkpoint_every: Duration,
}

impl SegmentedTransfer {
    /// Transfers every byte the table has not committed, then closes the writer (`sync_all`)
    /// and commits the final checkpoint. On a stop or failure every worker is halted, what
    /// is durable is checkpointed, and the writer is closed. Returns the URL of the first
    /// accepted response.
    ///
    /// # Errors
    /// The failure of the first worker that failed, a stop request, a pause for a full
    /// disk, or a writer or storage failure.
    pub async fn run(
        &self,
        table: SharedTable,
        mut writer: Writer,
    ) -> Result<Option<Url>, TaskStop> {
        let outcome = self.supervise(&table, &mut writer).await;
        match outcome {
            Ok(final_url) => {
                writer.close().await.map_err(|e| writer_stop(&e))?;
                if !table
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_fully_written()
                {
                    tracing::error!("segmented transfer ended with unwritten bytes");
                    return Err(Failure::new(DownloadErrorCode::Internal).into());
                }
                commit_segments(self.store.as_ref(), self.id, &table, &self.live).await?;
                Ok(final_url)
            }
            Err(stop) => {
                let flushed = flush_then_commit_segments(
                    &mut writer,
                    self.store.as_ref(),
                    self.id,
                    &table,
                    &self.live,
                )
                .await;
                if let Err(err) = flushed {
                    tracing::warn!(?err, "could not checkpoint after a stop");
                }
                if let Err(err) = writer.close().await {
                    tracing::warn!(error = %err, "writer did not close cleanly after a stop");
                }
                Err(stop)
            }
        }
    }

    async fn supervise(
        &self,
        table: &SharedTable,
        writer: &mut Writer,
    ) -> Result<Option<Url>, TaskStop> {
        let stop = self.live.stop();
        let shared = Arc::new(Shared::new(
            Arc::clone(table),
            writer.handle(),
            Arc::clone(&self.live),
            self.client.clone(),
            self.url.clone(),
            self.identity.clone(),
            stop.token().child_token(),
        ));
        let mut pool = Pool::new(Arc::clone(&shared));
        let result = self.event_loop(&shared, &mut pool, writer).await;
        shared.halt.cancel();
        pool.drain().await;
        self.live.set_connections(0);
        result.map(|()| shared.final_url())
    }

    async fn event_loop(
        &self,
        shared: &Arc<Shared>,
        pool: &mut Pool,
        writer: &mut Writer,
    ) -> Result<(), TaskStop> {
        let stop = self.live.stop();
        let mut cap_rx = self.cap.subscribe();
        let mut tick = interval(REBALANCE_TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut next_checkpoint = Instant::now() + self.checkpoint_every;
        let mut next_space_check = shared.lock().accepted_bytes() + FREE_SPACE_RECHECK;
        loop {
            pool.rebalance(self.cap.get());
            self.live
                .set_connections(u32::try_from(pool.alive()).unwrap_or(u32::MAX));
            if pool.is_empty() {
                if shared.lock().is_complete() {
                    return Ok(());
                }
                tracing::error!("no worker is running and work remains");
                return Err(Failure::new(DownloadErrorCode::Internal).into());
            }
            tokio::select! {
                biased;
                () = stop.cancelled() => return Err(stop.stopped()),
                () = shared.halt.cancelled() => {
                    return Err(shared.take_fatal().unwrap_or_else(|| {
                        TaskStop::Fail(Failure::new(DownloadErrorCode::Internal))
                    }));
                }
                Some(ended) = pool.next_end() => pool.on_end(ended),
                Ok(()) = cap_rx.changed() => {}
                _ = tick.tick() => {
                    if Instant::now() >= next_checkpoint {
                        flush_then_commit_segments(
                            writer, self.store.as_ref(), self.id, shared.table.as_ref(), &self.live,
                        ).await?;
                        next_checkpoint = Instant::now() + self.checkpoint_every;
                        next_space_check = self.check_space(shared, next_space_check)?;
                    }
                }
            }
        }
    }

    /// Re-checks free space once every [`FREE_SPACE_RECHECK`] accepted bytes.
    fn check_space(&self, shared: &Shared, next_check: u64) -> Result<u64, TaskStop> {
        let (accepted, total) = {
            let table = shared.lock();
            (table.accepted_bytes(), table.total())
        };
        if accepted < next_check {
            return Ok(next_check);
        }
        ensure_free_space(&self.dir, total.saturating_sub(accepted))?;
        Ok(accepted.saturating_add(FREE_SPACE_RECHECK))
    }
}

/// A running worker as the supervisor sees it.
struct Slot {
    cancel: CancellationToken,
    retiring: bool,
}

/// The workers of one download.
struct Pool {
    shared: Arc<Shared>,
    set: JoinSet<(u32, WorkerEnd)>,
    slots: BTreeMap<u32, Slot>,
    next_id: u32,
}

impl Pool {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            set: JoinSet::new(),
            slots: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Workers that are not being retired.
    fn alive(&self) -> usize {
        self.slots.values().filter(|s| !s.retiring).count()
    }

    fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    async fn next_end(&mut self) -> Option<(u32, WorkerEnd)> {
        if self.set.is_empty() {
            return std::future::pending().await;
        }
        match self.set.join_next().await {
            Some(Ok(ended)) => Some(ended),
            Some(Err(err)) => {
                tracing::error!(error = %err, "a download worker panicked");
                self.shared
                    .fail(TaskStop::Fail(Failure::new(DownloadErrorCode::Internal)));
                None
            }
            None => None,
        }
    }

    fn on_end(&mut self, (id, end): (u32, WorkerEnd)) {
        self.slots.remove(&id);
        tracing::debug!(worker = id, ?end, "worker ended");
    }

    /// Starts workers while below `cap` and work exists, and retires the youngest while
    /// above it.
    fn rebalance(&mut self, cap: u32) {
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        let mut started = 0_u32;
        while self.alive() < cap && self.shared.lock().has_assignable() && !self.halted() {
            self.spawn(CONNECTION_STAGGER * started);
            started += 1;
        }
        while self.alive() > cap {
            self.retire_youngest();
        }
    }

    fn halted(&self) -> bool {
        self.shared.halt.is_cancelled()
    }

    fn spawn(&mut self, stagger: Duration) {
        let id = self.next_id;
        self.next_id += 1;
        let cancel = self.shared.halt.child_token();
        self.slots.insert(
            id,
            Slot {
                cancel: cancel.clone(),
                retiring: false,
            },
        );
        let shared = Arc::clone(&self.shared);
        self.set.spawn(async move {
            let end = run_worker(shared, id, cancel, stagger).await;
            (id, end)
        });
    }

    fn retire_youngest(&mut self) {
        if let Some((id, slot)) = self.slots.iter_mut().rev().find(|(_, s)| !s.retiring) {
            slot.retiring = true;
            slot.cancel.cancel();
            tracing::debug!(worker = *id, "retiring the youngest worker");
        }
    }

    /// Waits for every worker to end (they were told to halt).
    async fn drain(&mut self) {
        while let Some(joined) = self.set.join_next().await {
            if let Err(err) = joined {
                tracing::error!(error = %err, "a download worker panicked");
            }
        }
        self.slots.clear();
    }
}
