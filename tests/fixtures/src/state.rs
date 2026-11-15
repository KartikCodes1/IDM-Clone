//! Server-wide shared state: the request log and per-resource request counters.

use crate::log::RequestLog;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Thread-safe named counters used by the stateful flags (`fail_first`, `mutate_after`,
/// `range_200_after`, `ttl_requests`, redirect token issue).
#[derive(Debug, Default)]
pub struct Counters {
    inner: Mutex<HashMap<String, u64>>,
}

impl Counters {
    /// Increments the counter `key` and returns its value before the increment (0 first).
    pub fn bump(&self, key: &str) -> u64 {
        let mut map = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = map.entry(key.to_owned()).or_insert(0);
        let before = *slot;
        *slot = slot.saturating_add(1);
        before
    }
}

/// What every connection and request handler shares.
#[derive(Debug, Clone)]
pub struct Shared {
    /// The request log and concurrency tracker.
    pub log: RequestLog,
    /// Stateful-flag counters.
    pub counters: Arc<Counters>,
    /// The bound loopback port (used to build absolute redirect URLs).
    pub port: u16,
}

impl Shared {
    /// Fresh state for a server bound to `port`.
    #[must_use]
    pub fn new(log: RequestLog, port: u16) -> Self {
        Self {
            log,
            counters: Arc::new(Counters::default()),
            port,
        }
    }

    /// The counter key for `kind` on one resource (`t` key plus path), so different tests
    /// and different resources never interfere.
    #[must_use]
    pub fn key(kind: &str, test_key: Option<&str>, path: &str) -> String {
        format!("{kind}|{}|{path}", test_key.unwrap_or(""))
    }
}

#[cfg(test)]
mod tests {
    use super::{Counters, Shared};

    #[test]
    fn bump_returns_prior_value_per_key() {
        let c = Counters::default();
        assert_eq!(c.bump("a"), 0);
        assert_eq!(c.bump("a"), 1);
        assert_eq!(c.bump("b"), 0);
    }

    #[test]
    fn keys_separate_kind_test_and_path() {
        assert_ne!(
            Shared::key("x", Some("t"), "/p"),
            Shared::key("x", None, "/p")
        );
        assert_ne!(Shared::key("x", None, "/p"), Shared::key("y", None, "/p"));
    }
}
