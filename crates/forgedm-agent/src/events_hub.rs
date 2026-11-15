//! The StateHub actor (ARCHITECTURE 3.6, ADR-019).
//!
//! A single task owns the download summary map, the event sequence counter, a ring of the
//! last [`EVENT_RING`] events and the subscribers. Every request is answered in one actor
//! turn without awaiting, so `Subscribe` registers a subscriber and replays
//! `(after_seq, current]` atomically: no event published between a client's `GetSnapshot`
//! and its `Subscribe` can be lost.
//!
//! The hub persists nothing. The engine publishes a change only after it is committed; the
//! hub then assigns the next `seq`. This slice has no engine, so [`HubHandle::publish`] is
//! driven by tests and, later, by the S1.7 engine.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use forgedm_ipc::{EventSink, OUTBOUND_QUEUE};
use forgedm_protocol::{
    AgentState, AgentStatusInfo, DownloadState, DownloadSummary, Event, EventBody,
    ListDownloadsResult, ProxySource, ResyncReason, SNAPSHOT_MAX, SnapshotResult, SubscribeRequest,
    SubscribeResult,
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::summary::holds_active_slot;

/// Events kept for replay (A.9 `EVENT_RING`).
pub const EVENT_RING: usize = 1024;
/// Capacity of the engine-to-hub channel (A.9 `ENGINE_TO_HUB`).
pub const ENGINE_TO_HUB: usize = 1024;

/// The hub stopped; nothing more can be published or asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HubError {
    /// The actor task has ended.
    #[error("the state hub is not running")]
    Gone,
}

/// What a `Subscribe` came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeOutcome {
    /// Registered; the replay (if any) is already queued on the sink.
    Subscribed(SubscribeResult),
    /// The position cannot be served; the client must `GetSnapshot` and subscribe again.
    Resync(ResyncReason),
}

/// Constant facts the hub reports in `AgentStatusInfo`.
#[derive(Debug, Clone)]
pub struct HubInit {
    /// New per agent start (UUIDv7).
    pub instance_id: Uuid,
    /// Effective proxy source.
    pub proxy_source: ProxySource,
    /// Data root shown in the status.
    pub data_dir: String,
    /// When the agent started (uptime base).
    pub started: Instant,
    /// Process id shown in the status.
    pub pid: u32,
}

pub(crate) enum Msg {
    Publish(EventBody),
    SetState(AgentState),
    Snapshot(oneshot::Sender<SnapshotResult>),
    Status(oneshot::Sender<AgentStatusInfo>),
    List {
        states: Option<Vec<DownloadState>>,
        limit: u32,
        offset: u32,
        reply: oneshot::Sender<ListDownloadsResult>,
    },
    Subscribe {
        request: SubscribeRequest,
        sink: EventSink,
        reply: oneshot::Sender<SubscribeOutcome>,
    },
}

/// A cheap, cloneable handle to the hub actor.
#[derive(Debug, Clone)]
pub struct HubHandle {
    tx: mpsc::Sender<Msg>,
    instance_id: Uuid,
}

impl HubHandle {
    /// The agent instance id (new per start).
    #[must_use]
    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    /// Publishes a committed change and waits for channel space (state events are never
    /// dropped). The hub assigns the `seq`.
    ///
    /// # Errors
    /// [`HubError::Gone`] when the hub has stopped.
    pub async fn publish(&self, body: EventBody) -> Result<(), HubError> {
        self.tx
            .send(Msg::Publish(body))
            .await
            .map_err(|_| HubError::Gone)
    }

    /// Publishes without waiting; for events that may be dropped under pressure (progress).
    /// Returns whether the event was queued.
    #[must_use]
    pub fn try_publish(&self, body: EventBody) -> bool {
        self.tx.try_send(Msg::Publish(body)).is_ok()
    }

    /// Changes the lifecycle state and broadcasts `AgentStatus`.
    ///
    /// # Errors
    /// [`HubError::Gone`] when the hub has stopped.
    pub async fn set_state(&self, state: AgentState) -> Result<(), HubError> {
        self.tx
            .send(Msg::SetState(state))
            .await
            .map_err(|_| HubError::Gone)
    }

    /// The current status.
    ///
    /// # Errors
    /// [`HubError::Gone`] when the hub has stopped.
    pub async fn status(&self) -> Result<AgentStatusInfo, HubError> {
        self.ask(Msg::Status).await
    }

    /// The snapshot: newest [`SNAPSHOT_MAX`] downloads and the `seq` they are consistent with.
    ///
    /// # Errors
    /// [`HubError::Gone`] when the hub has stopped.
    pub async fn snapshot(&self) -> Result<SnapshotResult, HubError> {
        self.ask(Msg::Snapshot).await
    }

    /// One page of downloads, newest first, with the total number matching.
    ///
    /// # Errors
    /// [`HubError::Gone`] when the hub has stopped.
    pub async fn list(
        &self,
        states: Option<Vec<DownloadState>>,
        limit: u32,
        offset: u32,
    ) -> Result<ListDownloadsResult, HubError> {
        self.ask(|reply| Msg::List {
            states,
            limit,
            offset,
            reply,
        })
        .await
    }

    /// Registers `sink` and queues the replay `(after_seq, current]` on it.
    ///
    /// # Errors
    /// [`HubError::Gone`] when the hub has stopped.
    pub async fn subscribe(
        &self,
        request: SubscribeRequest,
        sink: EventSink,
    ) -> Result<SubscribeOutcome, HubError> {
        self.ask(|reply| Msg::Subscribe {
            request,
            sink,
            reply,
        })
        .await
    }

    async fn ask<T>(&self, build: impl FnOnce(oneshot::Sender<T>) -> Msg) -> Result<T, HubError> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(build(reply))
            .await
            .map_err(|_| HubError::Gone)?;
        answer.await.map_err(|_| HubError::Gone)
    }

    /// A handle whose actor never runs (the returned receiver keeps the channel open), so
    /// every `ask` waits forever. For timeout tests.
    #[cfg(test)]
    pub(crate) fn unresponsive() -> (Self, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel(ENGINE_TO_HUB);
        (
            Self {
                tx,
                instance_id: Uuid::now_v7(),
            },
            rx,
        )
    }
}

/// Starts the hub actor with the downloads loaded at startup and returns its handle.
///
/// Must run inside a tokio runtime. The actor ends when every handle is dropped.
#[must_use]
pub fn spawn(init: HubInit, initial: Vec<DownloadSummary>) -> HubHandle {
    let (tx, rx) = mpsc::channel(ENGINE_TO_HUB);
    let handle = HubHandle {
        tx,
        instance_id: init.instance_id,
    };
    let hub = StateHub::new(init, initial);
    tokio::spawn(hub.run(rx));
    handle
}

struct StateHub {
    init: HubInit,
    state: AgentState,
    summaries: HashMap<Uuid, DownloadSummary>,
    seq: u64,
    ring: VecDeque<Event>,
    subscribers: Vec<EventSink>,
}

impl StateHub {
    fn new(init: HubInit, initial: Vec<DownloadSummary>) -> Self {
        Self {
            init,
            state: AgentState::Starting,
            summaries: initial.into_iter().map(|d| (d.id, d)).collect(),
            seq: 0,
            ring: VecDeque::with_capacity(EVENT_RING),
            subscribers: Vec::new(),
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        while let Some(msg) = rx.recv().await {
            self.handle(msg);
        }
    }

    /// One actor turn. Nothing in here awaits.
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Publish(body) => self.record(body),
            Msg::SetState(state) => {
                if self.state == state {
                    return;
                }
                self.state = state;
                self.record(EventBody::AgentStatus(self.status()));
            }
            Msg::Snapshot(reply) => {
                let _ = reply.send(self.snapshot());
            }
            Msg::Status(reply) => {
                let _ = reply.send(self.status());
            }
            Msg::List {
                states,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(self.list(states.as_deref(), limit, offset));
            }
            Msg::Subscribe {
                request,
                sink,
                reply,
            } => {
                let _ = reply.send(self.subscribe(&request, sink));
            }
        }
    }

    /// Applies a committed change, assigns the next `seq`, keeps it for replay and fans it
    /// out. A subscriber whose queue is full or gone is dropped (the connection layer then
    /// sends `ResyncRequired{lagged}` or just ends the subscription).
    fn record(&mut self, body: EventBody) {
        self.apply(&body);
        self.seq += 1;
        let event = Event {
            seq: self.seq,
            body,
        };
        if self.ring.len() == EVENT_RING {
            self.ring.pop_front();
        }
        self.ring.push_back(event.clone());
        let before = self.subscribers.len();
        self.subscribers
            .retain_mut(|sink| sink.is_open() && sink.try_send(event.clone()).is_ok());
        let dropped = before - self.subscribers.len();
        if dropped > 0 {
            tracing::debug!(dropped, "event subscribers dropped (closed or lagging)");
        }
    }

    fn apply(&mut self, body: &EventBody) {
        match body {
            EventBody::DownloadAdded(summary) | EventBody::DownloadChanged(summary) => {
                self.summaries.insert(summary.id, (**summary).clone());
            }
            EventBody::DownloadRemoved(removed) => {
                self.summaries.remove(&removed.id);
            }
            EventBody::AgentStatus(_)
            | EventBody::ProgressSnapshot(_)
            | EventBody::DownloadCompleted(_)
            | EventBody::DownloadFailed(_)
            | EventBody::QueueChanged(_)
            | EventBody::ResyncRequired(_) => {}
        }
    }

    fn subscribe(&mut self, request: &SubscribeRequest, mut sink: EventSink) -> SubscribeOutcome {
        self.subscribers.retain(EventSink::is_open);
        if request.agent_instance_id != self.init.instance_id || request.after_seq > self.seq {
            return SubscribeOutcome::Resync(ResyncReason::InstanceChanged);
        }
        let oldest = self.ring.front().map_or(self.seq + 1, |event| event.seq);
        if request.after_seq + 1 < oldest {
            return SubscribeOutcome::Resync(ResyncReason::Evicted);
        }
        let replay: Vec<&Event> = self
            .ring
            .iter()
            .filter(|event| event.seq > request.after_seq)
            .collect();
        if replay.len() > OUTBOUND_QUEUE {
            return SubscribeOutcome::Resync(ResyncReason::Lagged);
        }
        for event in replay {
            if sink.try_send(event.clone()).is_err() {
                return SubscribeOutcome::Resync(ResyncReason::Lagged);
            }
        }
        self.subscribers.push(sink);
        SubscribeOutcome::Subscribed(SubscribeResult {
            current_seq: self.seq,
        })
    }

    fn newest_first(&self) -> Vec<&DownloadSummary> {
        let mut all: Vec<&DownloadSummary> = self.summaries.values().collect();
        all.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        all
    }

    fn snapshot(&self) -> SnapshotResult {
        let all = self.newest_first();
        SnapshotResult {
            agent_instance_id: self.init.instance_id,
            seq: self.seq,
            agent_status: self.status(),
            truncated: all.len() > SNAPSHOT_MAX,
            downloads: all.into_iter().take(SNAPSHOT_MAX).cloned().collect(),
        }
    }

    fn list(
        &self,
        states: Option<&[DownloadState]>,
        limit: u32,
        offset: u32,
    ) -> ListDownloadsResult {
        let matching: Vec<&DownloadSummary> = self
            .newest_first()
            .into_iter()
            .filter(|d| states.is_none_or(|wanted| wanted.contains(&d.state)))
            .collect();
        let items = matching
            .iter()
            .skip(offset as usize)
            .take(limit as usize)
            .map(|d| (*d).clone())
            .collect();
        ListDownloadsResult {
            items,
            total: u32::try_from(matching.len()).unwrap_or(u32::MAX),
        }
    }

    fn status(&self) -> AgentStatusInfo {
        let count = |wanted: fn(DownloadState) -> bool| {
            let n = self.summaries.values().filter(|d| wanted(d.state)).count();
            u32::try_from(n).unwrap_or(u32::MAX)
        };
        let speed = self
            .summaries
            .values()
            .filter(|d| holds_active_slot(d.state))
            .filter_map(|d| d.speed_bps)
            .fold(0u64, u64::saturating_add);
        AgentStatusInfo {
            agent_version: env!("CARGO_PKG_VERSION").to_owned(),
            agent_pid: self.init.pid,
            agent_instance_id: self.init.instance_id,
            state: self.state,
            uptime_ms: u64::try_from(self.init.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            active: count(holds_active_slot),
            queued: count(|s| s == DownloadState::Queued),
            total_speed_bps: speed,
            proxy_source: self.init.proxy_source,
            data_dir: self.init.data_dir.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use forgedm_ipc::{SubscriptionItem, event_channel};
    use forgedm_protocol::{ClientKind, DownloadRemovedData, Priority, ResumeSupport};

    use super::*;

    fn init() -> HubInit {
        HubInit {
            instance_id: Uuid::now_v7(),
            proxy_source: ProxySource::None,
            data_dir: r"C:\data".to_owned(),
            started: Instant::now(),
            pid: 42,
        }
    }

    fn summary(n: u32, state: DownloadState) -> DownloadSummary {
        DownloadSummary {
            id: Uuid::from_u128(u128::from(n) + 1),
            display_name: format!("f{n}"),
            url_display: "https://example.com/x".to_owned(),
            state,
            state_reason: None,
            priority: Priority::Normal,
            source: ClientKind::Cli,
            target_dir: r"C:\d".to_owned(),
            final_path: None,
            total_bytes: None,
            accepted_bytes: 0,
            committed_bytes: 0,
            speed_bps: None,
            eta_ms: None,
            transfer_mode: None,
            resume: ResumeSupport::Unknown,
            last_error: None,
            created_at_ms: u64::from(n),
            completed_at_ms: None,
        }
    }

    fn added(n: u32, state: DownloadState) -> EventBody {
        EventBody::DownloadAdded(Box::new(summary(n, state)))
    }

    fn sub(hub: &StateHub, after_seq: u64) -> SubscribeRequest {
        SubscribeRequest {
            agent_instance_id: hub.init.instance_id,
            after_seq,
        }
    }

    #[test]
    fn seq_strictly_increases_and_updates_the_map() {
        let mut hub = StateHub::new(init(), Vec::new());
        hub.record(added(1, DownloadState::Queued));
        hub.record(EventBody::DownloadChanged(Box::new(summary(
            1,
            DownloadState::Downloading,
        ))));
        hub.record(added(2, DownloadState::Queued));
        hub.record(EventBody::DownloadRemoved(DownloadRemovedData {
            id: summary(2, DownloadState::Queued).id,
        }));

        let seqs: Vec<u64> = hub.ring.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4]);
        let snap = hub.snapshot();
        assert_eq!(snap.seq, 4);
        assert_eq!(snap.downloads.len(), 1);
        assert_eq!(snap.downloads[0].state, DownloadState::Downloading);
    }

    #[test]
    fn ring_keeps_only_the_last_1024_events() {
        let mut hub = StateHub::new(init(), Vec::new());
        for n in 0..1100 {
            hub.record(added(n, DownloadState::Queued));
        }
        assert_eq!(hub.ring.len(), EVENT_RING);
        assert_eq!(hub.ring.front().unwrap().seq, 77);
        assert_eq!(hub.ring.back().unwrap().seq, 1100);
    }

    #[test]
    fn subscribe_boundaries() {
        let mut hub = StateHub::new(init(), Vec::new());
        for n in 0..1100 {
            hub.record(added(n, DownloadState::Queued));
        }
        let outcome = |hub: &mut StateHub, after_seq| {
            let (sink, _rx) = event_channel();
            let request = sub(hub, after_seq);
            hub.subscribe(&request, sink)
        };
        // Evicted: needs seq 1 .. but the ring starts at 77.
        assert_eq!(
            outcome(&mut hub, 0),
            SubscribeOutcome::Resync(ResyncReason::Evicted)
        );
        assert_eq!(
            outcome(&mut hub, 75),
            SubscribeOutcome::Resync(ResyncReason::Evicted)
        );
        // Oldest replayable position, but more than the outbound queue holds.
        assert_eq!(
            outcome(&mut hub, 76),
            SubscribeOutcome::Resync(ResyncReason::Lagged)
        );
        // Exactly the outbound queue size is fine.
        let after = 1100 - u64::try_from(OUTBOUND_QUEUE).unwrap();
        assert!(matches!(
            outcome(&mut hub, after),
            SubscribeOutcome::Subscribed(_)
        ));
        // Up to date, and a position from the future.
        assert!(matches!(
            outcome(&mut hub, 1100),
            SubscribeOutcome::Subscribed(_)
        ));
        assert_eq!(
            outcome(&mut hub, 1101),
            SubscribeOutcome::Resync(ResyncReason::InstanceChanged)
        );
    }

    #[test]
    fn wrong_instance_requires_resync() {
        let mut hub = StateHub::new(init(), Vec::new());
        let (sink, _rx) = event_channel();
        let request = SubscribeRequest {
            agent_instance_id: Uuid::now_v7(),
            after_seq: 0,
        };
        assert_eq!(
            hub.subscribe(&request, sink),
            SubscribeOutcome::Resync(ResyncReason::InstanceChanged)
        );
        assert!(hub.subscribers.is_empty());
    }

    #[tokio::test]
    async fn replay_then_live_events_arrive_in_order_without_gaps() {
        let mut hub = StateHub::new(init(), Vec::new());
        for n in 0..5 {
            hub.record(added(n, DownloadState::Queued));
        }
        let (sink, mut rx) = event_channel();

        let request = sub(&hub, 2);
        let outcome = hub.subscribe(&request, sink);
        hub.record(added(9, DownloadState::Queued));

        assert_eq!(
            outcome,
            SubscribeOutcome::Subscribed(SubscribeResult { current_seq: 5 })
        );
        for expected in 3..=6 {
            let SubscriptionItem::Event(event) = rx.recv().await else {
                unreachable!("expected an event");
            };
            assert_eq!(event.seq, expected);
        }
    }

    #[tokio::test]
    async fn a_subscriber_that_never_reads_is_dropped_and_told_to_resync() {
        let mut hub = StateHub::new(init(), Vec::new());
        let (sink, mut rx) = event_channel();
        let request = sub(&hub, 0);
        assert!(matches!(
            hub.subscribe(&request, sink),
            SubscribeOutcome::Subscribed(_)
        ));

        for n in 0..=u32::try_from(OUTBOUND_QUEUE).unwrap() {
            hub.record(added(n, DownloadState::Queued));
        }

        assert!(hub.subscribers.is_empty(), "lagging subscriber is dropped");
        let mut delivered = 0;
        loop {
            match rx.recv().await {
                SubscriptionItem::Event(_) => delivered += 1,
                SubscriptionItem::Resync(_) => break,
                SubscriptionItem::Closed => unreachable!("expected a resync"),
            }
        }
        assert_eq!(delivered, OUTBOUND_QUEUE);
    }

    #[test]
    fn closed_subscribers_are_pruned_on_the_next_subscribe() {
        let mut hub = StateHub::new(init(), Vec::new());
        let request = sub(&hub, 0);
        let (sink, rx) = event_channel();
        hub.subscribe(&request, sink);
        drop(rx);
        let (sink, _keep) = event_channel();
        hub.subscribe(&request, sink);
        assert_eq!(hub.subscribers.len(), 1);
    }

    #[test]
    fn list_filters_orders_newest_first_and_pages() {
        let initial = vec![
            summary(1, DownloadState::Completed),
            summary(2, DownloadState::Queued),
            summary(3, DownloadState::Completed),
            summary(4, DownloadState::Paused),
        ];
        let hub = StateHub::new(init(), initial);

        let all = hub.list(None, 10, 0);
        assert_eq!(all.total, 4);
        let created: Vec<u64> = all.items.iter().map(|d| d.created_at_ms).collect();
        assert_eq!(created, [4, 3, 2, 1]);

        let done = hub.list(Some(&[DownloadState::Completed]), 1, 1);
        assert_eq!(done.total, 2);
        assert_eq!(done.items.len(), 1);
        assert_eq!(done.items[0].created_at_ms, 1);

        assert_eq!(hub.list(Some(&[]), 10, 0).total, 0);
        assert!(hub.list(None, 10, 99).items.is_empty());
    }

    #[test]
    fn snapshot_is_capped_at_1000_and_flags_truncation() {
        let initial: Vec<_> = (0..1005)
            .map(|n| summary(n, DownloadState::Completed))
            .collect();
        let hub = StateHub::new(init(), initial);
        let snap = hub.snapshot();
        assert_eq!(snap.downloads.len(), SNAPSHOT_MAX);
        assert!(snap.truncated);
        assert_eq!(snap.downloads[0].created_at_ms, 1004);
    }

    #[test]
    fn status_counts_active_and_queued_and_sums_speed() {
        let mut fast = summary(1, DownloadState::Downloading);
        fast.speed_bps = Some(300);
        let mut probing = summary(2, DownloadState::Probing);
        probing.speed_bps = Some(20);
        let mut idle = summary(5, DownloadState::Paused);
        idle.speed_bps = Some(9_999);
        let hub = StateHub::new(
            init(),
            vec![
                fast,
                probing,
                idle,
                summary(3, DownloadState::Queued),
                summary(4, DownloadState::Queued),
            ],
        );

        let status = hub.status();

        assert_eq!(
            (status.active, status.queued, status.total_speed_bps),
            (2, 2, 320)
        );
        assert_eq!(status.state, AgentState::Starting);
        assert_eq!(status.agent_pid, 42);
    }

    #[test]
    fn set_state_to_the_current_state_is_a_no_op() {
        let mut hub = StateHub::new(init(), Vec::new());
        hub.handle(Msg::SetState(AgentState::ShuttingDown));
        hub.handle(Msg::SetState(AgentState::ShuttingDown));
        assert_eq!(hub.seq, 1);
    }

    #[test]
    fn closed_subscribers_are_pruned_on_the_next_record() {
        let mut hub = StateHub::new(init(), Vec::new());
        let (sink, rx) = event_channel();
        hub.subscribers.push(sink);
        drop(rx);
        hub.record(added(1, DownloadState::Queued));
        assert!(hub.subscribers.is_empty());
    }

    #[test]
    fn set_state_broadcasts_an_agent_status_event() {
        let mut hub = StateHub::new(init(), Vec::new());
        hub.handle(Msg::SetState(AgentState::Ready));
        let event = hub.ring.back().unwrap();
        assert_eq!(event.seq, 1);
        let EventBody::AgentStatus(status) = &event.body else {
            unreachable!("expected AgentStatus");
        };
        assert_eq!(status.state, AgentState::Ready);
    }

    #[tokio::test]
    async fn handle_answers_through_the_actor() {
        let handle = spawn(init(), vec![summary(1, DownloadState::Queued)]);
        assert_eq!(handle.list(None, 5, 0).await.unwrap().total, 1);
        assert!(
            handle
                .publish(added(2, DownloadState::Queued))
                .await
                .is_ok()
        );
        assert_eq!(handle.snapshot().await.unwrap().seq, 1);
        assert!(handle.try_publish(added(3, DownloadState::Queued)));
        assert_eq!(handle.snapshot().await.unwrap().downloads.len(), 3);
    }
}
