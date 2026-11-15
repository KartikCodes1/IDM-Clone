//! Request log shared between the server and tests.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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
}

/// A cloneable handle to the shared request log.
#[derive(Debug, Clone, Default)]
pub struct RequestLog {
    inner: Arc<Mutex<Vec<LoggedRequest>>>,
}

impl RequestLog {
    fn guard(&self) -> MutexGuard<'_, Vec<LoggedRequest>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends an entry, assigning its `seq`.
    pub fn record(&self, mut entry: LoggedRequest) {
        let mut entries = self.guard();
        entry.seq = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        entries.push(entry);
    }

    /// A snapshot of every entry in arrival order.
    #[must_use]
    pub fn entries(&self) -> Vec<LoggedRequest> {
        self.guard().clone()
    }

    /// A snapshot of the entries tagged with `key`.
    #[must_use]
    pub fn for_test(&self, key: &str) -> Vec<LoggedRequest> {
        self.guard()
            .iter()
            .filter(|e| e.test_key.as_deref() == Some(key))
            .cloned()
            .collect()
    }

    /// Number of recorded requests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.guard().len()
    }

    /// True when nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.guard().is_empty()
    }

    /// Discards all entries.
    pub fn clear(&self) {
        self.guard().clear();
    }
}
