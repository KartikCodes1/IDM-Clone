//! Request log shared between the server and tests, plus the concurrency counter.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The log keeps at most this many entries; the oldest are discarded first, so a long-running
/// fixture cannot grow without bound. `seq` keeps counting across discards.
pub const MAX_LOG_ENTRIES: usize = 50_000;

/// One request as seen by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRequest {
    /// Arrival order, starting at 0.
    pub seq: u64,
    /// HTTP method.
    pub method: String,
    /// Request target (path and query).
    pub target: String,
    /// Raw `Range` header, if any.
    pub range: Option<String>,
    /// Raw `If-Range` header, if any.
    pub if_range: Option<String>,
    /// Server-assigned id of the TCP connection that carried the request.
    pub conn_id: u64,
    /// The `t=` test key, if any.
    pub test_key: Option<String>,
    /// Arrival time, unix milliseconds.
    pub at_unix_ms: u64,
    /// Remote TCP port of the connection (the client side of the loopback pair).
    pub peer_port: u16,
    /// Whether an `Authorization` header was present. The value is never recorded.
    pub has_authorization: bool,
    /// Whether a `Cookie` header was present. The value is never recorded.
    pub has_cookie: bool,
}

#[derive(Debug, Default)]
struct Inner {
    entries: VecDeque<LoggedRequest>,
    next_seq: u64,
    in_flight: u64,
    max_in_flight: u64,
}

/// A cloneable handle to the shared request log.
#[derive(Debug, Clone, Default)]
pub struct RequestLog {
    inner: Arc<Mutex<Inner>>,
}

/// Holds one concurrency slot; the slot is released when this is dropped.
#[derive(Debug)]
pub struct InFlight {
    inner: Arc<Mutex<Inner>>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.in_flight = inner.in_flight.saturating_sub(1);
    }
}

impl RequestLog {
    fn guard(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends an entry, assigning its `seq`.
    pub fn record(&self, mut entry: LoggedRequest) {
        let mut inner = self.guard();
        entry.seq = inner.next_seq;
        inner.next_seq = inner.next_seq.saturating_add(1);
        if inner.entries.len() >= MAX_LOG_ENTRIES {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry);
    }

    /// A snapshot of every entry in arrival order.
    #[must_use]
    pub fn entries(&self) -> Vec<LoggedRequest> {
        self.guard().entries.iter().cloned().collect()
    }

    /// A snapshot of the entries tagged with `key`.
    #[must_use]
    pub fn for_test(&self, key: &str) -> Vec<LoggedRequest> {
        self.guard()
            .entries
            .iter()
            .filter(|e| e.test_key.as_deref() == Some(key))
            .cloned()
            .collect()
    }

    /// Number of recorded requests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.guard().entries.len()
    }

    /// True when nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.guard().entries.is_empty()
    }

    /// Discards all entries (the concurrency high-water mark is kept).
    pub fn clear(&self) {
        self.guard().entries.clear();
    }

    /// Takes a concurrency slot, or `None` when `limit` slots are already held.
    ///
    /// Every successful call raises the high-water mark if needed.
    #[must_use]
    pub fn try_enter(&self, limit: Option<u64>) -> Option<InFlight> {
        let mut inner = self.guard();
        if limit.is_some_and(|max| inner.in_flight >= max) {
            return None;
        }
        inner.in_flight += 1;
        inner.max_in_flight = inner.max_in_flight.max(inner.in_flight);
        Some(InFlight {
            inner: Arc::clone(&self.inner),
        })
    }

    /// The most requests ever in flight at once. A request is in flight until its response
    /// body is finished or dropped.
    #[must_use]
    pub fn max_concurrency(&self) -> u64 {
        self.guard().max_in_flight
    }

    /// Requests in flight right now.
    #[must_use]
    pub fn current_concurrency(&self) -> u64 {
        self.guard().in_flight
    }
}

#[cfg(test)]
mod tests {
    use super::{LoggedRequest, MAX_LOG_ENTRIES, RequestLog};

    fn entry() -> LoggedRequest {
        LoggedRequest {
            seq: 0,
            method: "GET".to_owned(),
            target: "/f/1/1".to_owned(),
            range: None,
            if_range: None,
            conn_id: 0,
            test_key: None,
            at_unix_ms: 0,
            peer_port: 0,
            has_authorization: false,
            has_cookie: false,
        }
    }

    #[test]
    fn log_is_bounded_and_keeps_counting_seq() {
        let log = RequestLog::default();
        for _ in 0..MAX_LOG_ENTRIES + 10 {
            log.record(entry());
        }
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
        let all = log.entries();
        assert_eq!(all[0].seq, 10, "oldest entries were discarded");
        assert_eq!(
            all.last().map(|e| e.seq),
            Some(u64::try_from(MAX_LOG_ENTRIES + 9).unwrap_or(0))
        );
    }
}
