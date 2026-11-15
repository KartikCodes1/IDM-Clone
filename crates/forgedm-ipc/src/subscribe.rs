//! Per-connection subscription plumbing (ARCHITECTURE 3.6).
//!
//! The `StateHub` actor lives in the agent. This module only provides the bounded outbound
//! queue between the hub and one connection: the hub holds an [`EventSink`] and pushes
//! events with [`EventSink::try_send`] (never blocking); the connection task drains the
//! matching [`EventReceiver`]. When the queue is full the sink is dropped and the receiver
//! yields every queued event followed by one `ResyncRequired{lagged}`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use forgedm_protocol::{Event, EventBody, ResyncReason, ResyncRequiredData};
use tokio::sync::mpsc::{self, error::TrySendError};

use crate::OUTBOUND_QUEUE;

/// Why an event could not be queued. Either way the hub must drop the sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SinkError {
    /// The queue was full: this send dropped the subscription and the client will be told
    /// to resynchronise.
    #[error("subscriber lagged and was dropped")]
    Lagged,
    /// The subscription is already over (lagged earlier, or the connection went away).
    #[error("subscription is closed")]
    Closed,
}

/// The hub's end of a subscription.
#[derive(Debug)]
pub struct EventSink {
    sender: Option<mpsc::Sender<Event>>,
    lagged: Arc<AtomicBool>,
}

/// The connection's end of a subscription.
#[derive(Debug)]
pub struct EventReceiver {
    receiver: mpsc::Receiver<Event>,
    lagged: Arc<AtomicBool>,
    /// Sequence number of the last event handed to the connection (0 if none).
    last_seq: u64,
}

/// What the connection should do next with a subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionItem {
    /// A queued event to forward.
    Event(Event),
    /// Forward this `ResyncRequired{lagged}` event, then end the subscription. Its `seq` is
    /// that of the last event delivered before it (0 if none).
    Resync(Event),
    /// The subscription ended without a lag (the hub dropped its sink).
    Closed,
}

/// Creates a subscription queue of [`OUTBOUND_QUEUE`] events.
#[must_use]
pub fn event_channel() -> (EventSink, EventReceiver) {
    let (sender, receiver) = mpsc::channel(OUTBOUND_QUEUE);
    let lagged = Arc::new(AtomicBool::new(false));
    (
        EventSink {
            sender: Some(sender),
            lagged: lagged.clone(),
        },
        EventReceiver {
            receiver,
            lagged,
            last_seq: 0,
        },
    )
}

impl EventSink {
    /// Queues `event` without waiting.
    ///
    /// # Errors
    /// [`SinkError::Lagged`] when the queue is full (the subscription is dropped and a
    /// resync will be sent), [`SinkError::Closed`] when it is already over.
    pub fn try_send(&mut self, event: Event) -> Result<(), SinkError> {
        let Some(sender) = &self.sender else {
            return Err(SinkError::Closed);
        };
        match sender.try_send(event) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.lagged.store(true, Ordering::SeqCst);
                self.sender = None;
                Err(SinkError::Lagged)
            }
            Err(TrySendError::Closed(_)) => {
                self.sender = None;
                Err(SinkError::Closed)
            }
        }
    }

    /// Whether the subscription is still accepting events.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.sender.as_ref().is_some_and(|s| !s.is_closed())
    }
}

impl EventReceiver {
    /// Waits for the next item. After [`SubscriptionItem::Resync`] or
    /// [`SubscriptionItem::Closed`] the subscription is over.
    pub async fn recv(&mut self) -> SubscriptionItem {
        match self.receiver.recv().await {
            Some(event) => {
                self.last_seq = event.seq;
                SubscriptionItem::Event(event)
            }
            None if self.lagged.load(Ordering::SeqCst) => {
                SubscriptionItem::Resync(resync_event(self.last_seq))
            }
            None => SubscriptionItem::Closed,
        }
    }
}

fn resync_event(seq: u64) -> Event {
    Event {
        seq,
        body: EventBody::ResyncRequired(ResyncRequiredData {
            reason: ResyncReason::Lagged,
        }),
    }
}
