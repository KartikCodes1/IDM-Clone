//! Per-host throttling state shared by every download (ARCHITECTURE 4.10, ADR-014).
//!
//! The key is `scheme://host:port` of the download's original URL. A 429/503 cooldown set by
//! one connection holds back every connection and every download to that host; the AIMD
//! connection limit halves once per burst of throttles and recovers after two clean minutes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;
use url::Url;

use crate::retry::HostState;

/// Most distinct hosts remembered; the registry is cleared when exceeded, which only forgets
/// cooldowns and AIMD reductions (never correctness).
const MAX_HOSTS: usize = 4096;

/// Connections allowed per host by default (A.9 `CONNS_PER_HOST`).
pub const CONNS_PER_HOST: u32 = 8;

/// The state of one host.
#[derive(Debug)]
pub struct HostHandle {
    state: Mutex<HostState>,
}

impl HostHandle {
    /// A host allowed `max_connections` connections.
    #[must_use]
    pub fn new(max_connections: u32) -> Self {
        Self {
            state: Mutex::new(HostState::new(max_connections)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The AIMD connection limit at `now` (after any recovery that is due).
    #[must_use]
    pub fn limit(&self, now: Instant) -> u32 {
        let mut state = self.lock();
        state.recover(now);
        state.connection_limit()
    }

    /// Time left of the cooldown at `now`.
    #[must_use]
    pub fn cooldown_remaining(&self, now: Instant) -> Duration {
        self.lock().cooldown_remaining(now)
    }

    /// A 429/503: cool down for `cooldown`. The first throttle of a burst also halves the
    /// connection limit; throttles that arrive while already cooling only extend the wait.
    pub fn throttled(&self, now: Instant, cooldown: Duration) {
        let mut state = self.lock();
        if state.is_blocked(now) {
            state.extend_cooldown(now, cooldown);
        } else {
            state.on_throttled(now, cooldown);
        }
    }

    /// A connection was refused at open: halve the limit (no cooldown).
    pub fn refused(&self, now: Instant) {
        self.lock().on_refused_at_open(now);
    }
}

/// The key hosts are shared by: `scheme://host:port`.
#[must_use]
pub fn host_key(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default().unwrap_or(0)
    )
}

/// Every host seen so far.
#[derive(Debug)]
pub struct HostRegistry {
    per_host: u32,
    map: Mutex<HashMap<String, Arc<HostHandle>>>,
}

impl Default for HostRegistry {
    fn default() -> Self {
        Self::new(CONNS_PER_HOST)
    }
}

impl HostRegistry {
    /// A registry whose hosts allow `per_host` connections (A.9 `CONNS_PER_HOST`).
    #[must_use]
    pub fn new(per_host: u32) -> Self {
        Self {
            per_host,
            map: Mutex::new(HashMap::new()),
        }
    }

    /// The shared state of the host of `url`.
    pub fn handle_for(&self, url: &Url) -> Arc<HostHandle> {
        let key = host_key(url);
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= MAX_HOSTS && !map.contains_key(&key) {
            map.clear();
        }
        Arc::clone(
            map.entry(key)
                .or_insert_with(|| Arc::new(HostHandle::new(self.per_host))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn hosts_are_shared_per_scheme_host_and_port() {
        let registry = HostRegistry::default();
        let a = registry.handle_for(&url("http://Example.test/a?x=1"));
        let b = registry.handle_for(&url("http://example.test:80/b"));
        let other_port = registry.handle_for(&url("http://example.test:8080/a"));
        let other_scheme = registry.handle_for(&url("https://example.test/a"));
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &other_port));
        assert!(!Arc::ptr_eq(&a, &other_scheme));
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_throttles_halves_once_and_keeps_the_longest_cooldown() {
        let host = HostHandle::new(8);
        let now = Instant::now();
        host.throttled(now, Duration::from_secs(5));
        host.throttled(now, Duration::from_secs(9));
        host.throttled(now, Duration::from_secs(2));
        assert_eq!(host.limit(now), 4);
        assert_eq!(host.cooldown_remaining(now), Duration::from_secs(9));
    }
}
