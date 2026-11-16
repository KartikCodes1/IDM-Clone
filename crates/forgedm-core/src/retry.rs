//! Retry classification and backoff policy (ARCHITECTURE 4.10, ADR-014).
//!
//! Everything here is pure: callers pass the current [`Instant`] (from an injected
//! [`Clock`](crate::clock::Clock)) and a seedable [`Jitter`], so behaviour is deterministic
//! under test. Constants come from PHASE_0_1_PLAN Appendix A.9.

use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};
use std::time::{Duration, SystemTime};

use forgedm_protocol::DownloadErrorCode;
use thiserror::Error;
use tokio::time::Instant;

/// Backoff base (A.9).
pub const RETRY_BASE: Duration = Duration::from_secs(1);
/// Backoff cap (A.9).
pub const RETRY_CAP: Duration = Duration::from_secs(60);
/// Consecutive no-progress failures allowed per segment (A.9).
pub const SEGMENT_BUDGET: u32 = 8;
/// Consecutive no-progress failures allowed per download (A.9).
pub const DOWNLOAD_BUDGET: u32 = 20;
/// Progress in bytes that resets a failure budget (A.9: 1 MiB).
pub const BUDGET_RESET_BYTES: u64 = 1024 * 1024;
/// Progress duration that resets a failure budget (A.9: 30 s).
pub const BUDGET_RESET_DURATION: Duration = Duration::from_secs(30);
/// Longest `Retry-After` honoured inline, while the worker stays attached (A.9).
pub const RETRY_AFTER_INLINE_MAX: Duration = Duration::from_secs(120);
/// Longest `Retry-After` honoured in `retry_waiting`; beyond it the download fails (A.9).
pub const RETRY_AFTER_WAIT_MAX: Duration = Duration::from_secs(3600);
/// Re-resolves allowed per download per [`RERESOLVE_WINDOW`] (A.9: 3 per hour).
pub const RERESOLVE_MAX: usize = 3;
/// Window for [`RERESOLVE_MAX`].
pub const RERESOLVE_WINDOW: Duration = Duration::from_secs(3600);
/// Clean time after which AIMD adds one connection back (A.9: 2 min).
pub const AIMD_RECOVERY_INTERVAL: Duration = Duration::from_secs(120);

/// Largest backoff exponent applied before the cap: `1 s * 2^31` already exceeds any cap.
const MAX_BACKOFF_SHIFT: u32 = 31;

// ---------------------------------------------------------------------------------------
// Jitter and backoff
// ---------------------------------------------------------------------------------------

/// Seedable pseudo-random source (splitmix64). Not cryptographic; used only for backoff.
#[derive(Debug, Clone)]
pub struct Jitter {
    state: u64,
}

impl Jitter {
    /// A deterministic generator: the same seed yields the same sequence.
    #[must_use]
    pub const fn seeded(seed: u64) -> Self {
        Self { state: seed }
    }

    /// A generator seeded from the standard library's per-process random keys.
    #[must_use]
    pub fn from_entropy() -> Self {
        Self::seeded(RandomState::new().hash_one(0_u64))
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform value in `0..=max`.
    pub fn up_to_inclusive(&mut self, max: u64) -> u64 {
        let raw = self.next_u64();
        if max == u64::MAX {
            return raw;
        }
        let scaled = (u128::from(raw) * (u128::from(max) + 1)) >> 64;
        u64::try_from(scaled).unwrap_or(max)
    }
}

/// Exponential backoff with full jitter: `rand(0, min(cap, base * 2^n))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    base: Duration,
    cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: RETRY_BASE,
            cap: RETRY_CAP,
        }
    }
}

impl RetryPolicy {
    /// A policy with explicit base and cap.
    #[must_use]
    pub const fn new(base: Duration, cap: Duration) -> Self {
        Self { base, cap }
    }

    /// Upper bound of the delay before retry number `attempt` (0-based).
    #[must_use]
    pub fn ceiling(&self, attempt: u32) -> Duration {
        let factor = 1_u32 << attempt.min(MAX_BACKOFF_SHIFT);
        self.base.saturating_mul(factor).min(self.cap)
    }

    /// A jittered delay in `[0, ceiling(attempt)]`.
    pub fn delay(&self, attempt: u32, jitter: &mut Jitter) -> Duration {
        let max = u64::try_from(self.ceiling(attempt).as_nanos()).unwrap_or(u64::MAX);
        Duration::from_nanos(jitter.up_to_inclusive(max))
    }
}

// ---------------------------------------------------------------------------------------
// Failure budgets
// ---------------------------------------------------------------------------------------

/// A failure budget ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("retry budget of {limit} consecutive failures exhausted")]
pub struct BudgetExhausted {
    /// The budget that was exhausted.
    pub limit: u32,
}

/// Counts consecutive no-progress failures; progress resets the count (A.9).
#[derive(Debug, Clone)]
pub struct RetryBudget {
    limit: u32,
    failures: u32,
    progress_bytes: u64,
    progress_since: Option<Instant>,
}

impl RetryBudget {
    /// A budget of `limit` consecutive failures.
    #[must_use]
    pub const fn new(limit: u32) -> Self {
        Self {
            limit,
            failures: 0,
            progress_bytes: 0,
            progress_since: None,
        }
    }

    /// The per-segment budget (8).
    #[must_use]
    pub const fn per_segment() -> Self {
        Self::new(SEGMENT_BUDGET)
    }

    /// The per-download budget (20).
    #[must_use]
    pub const fn per_download() -> Self {
        Self::new(DOWNLOAD_BUDGET)
    }

    /// Consecutive failures recorded so far.
    #[must_use]
    pub const fn failures(&self) -> u32 {
        self.failures
    }

    /// Records a failure and returns the 0-based retry attempt number to back off with.
    /// The failure that reaches the limit exhausts the budget.
    ///
    /// # Errors
    ///
    /// [`BudgetExhausted`] once `limit` consecutive failures have been recorded.
    pub fn on_failure(&mut self) -> Result<u32, BudgetExhausted> {
        self.progress_bytes = 0;
        self.progress_since = None;
        let attempt = self.failures;
        self.failures = self.failures.saturating_add(1);
        if self.failures >= self.limit {
            return Err(BudgetExhausted { limit: self.limit });
        }
        Ok(attempt)
    }

    /// Records `bytes` of progress at `now`. 1 MiB, or 30 s of progress since the first
    /// progress after a failure, resets the failure count.
    pub fn on_progress(&mut self, bytes: u64, now: Instant) {
        if self.failures == 0 {
            return;
        }
        let since = *self.progress_since.get_or_insert(now);
        self.progress_bytes = self.progress_bytes.saturating_add(bytes);
        let long_enough = now.saturating_duration_since(since) >= BUDGET_RESET_DURATION;
        if self.progress_bytes >= BUDGET_RESET_BYTES || long_enough {
            self.failures = 0;
            self.progress_bytes = 0;
            self.progress_since = None;
        }
    }
}

/// Limits URL re-resolves to [`RERESOLVE_MAX`] per [`RERESOLVE_WINDOW`] (A.9).
#[derive(Debug, Clone, Default)]
pub struct ReresolveLimiter {
    uses: VecDeque<Instant>,
}

impl ReresolveLimiter {
    /// An empty limiter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a re-resolve slot at `now` if one is free.
    pub fn try_acquire(&mut self, now: Instant) -> bool {
        while let Some(&oldest) = self.uses.front() {
            if now.saturating_duration_since(oldest) < RERESOLVE_WINDOW {
                break;
            }
            self.uses.pop_front();
        }
        if self.uses.len() >= RERESOLVE_MAX {
            return false;
        }
        self.uses.push_back(now);
        true
    }
}

// ---------------------------------------------------------------------------------------
// Retry-After
// ---------------------------------------------------------------------------------------

/// Parses a `Retry-After` value: delta-seconds, or an HTTP-date taken relative to the
/// response's own `Date` (`response_date`). Returns `None` for anything else, including an
/// HTTP-date when no response `Date` is available. A date in the past is a zero delay and an
/// overflowing delta saturates at [`Duration::MAX`].
#[must_use]
pub fn parse_retry_after(value: &str, response_date: Option<SystemTime>) -> Option<Duration> {
    let value = value.trim_matches([' ', '\t']);
    if value.is_empty() {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(
            value
                .parse::<u64>()
                .map_or(Duration::MAX, Duration::from_secs),
        );
    }
    let date = response_date?;
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(date).unwrap_or(Duration::ZERO))
}

/// Maps an honoured `Retry-After` delay to a disposition: up to 120 s wait inline, up to one
/// hour wait in `retry_waiting`, longer fails with `rate_limited`.
#[must_use]
pub fn retry_after_disposition(delay: Duration, now: Instant) -> Disposition {
    if delay <= RETRY_AFTER_INLINE_MAX {
        Disposition::RetryAfter { delay }
    } else if delay <= RETRY_AFTER_WAIT_MAX {
        Disposition::WaitUntil { at: now + delay }
    } else {
        Disposition::Fatal(DownloadErrorCode::RateLimited)
    }
}

// ---------------------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------------------

/// What the engine should do about a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Retry the segment from its current claim after the policy's backoff.
    Retry,
    /// Retry after exactly this server-provided delay (inline).
    RetryAfter {
        /// How long to wait.
        delay: Duration,
    },
    /// Park in `retry_waiting` until this instant.
    WaitUntil {
        /// When to retry.
        at: Instant,
    },
    /// Throttled without a usable `Retry-After`: cool the host down (backoff delay) and
    /// apply AIMD. Every 429/503 outcome implies the same host bookkeeping.
    Cooldown,
    /// 401/407: the user must supply credentials.
    NeedsCredentials,
    /// 403/404/410 during transfer: re-resolve the URL once (see [`ReresolveLimiter`]),
    /// otherwise the download needs a URL refresh.
    NeedsUrlRefresh,
    /// Local disk is full: pause (`disk_full`).
    PauseDiskFull,
    /// Destination volume or folder went away: pause (`destination_unavailable`).
    PauseDestinationUnavailable,
    /// Fail with this code.
    Fatal(DownloadErrorCode),
}

/// Where a failing request happened; a stale URL only matters mid-transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// HEAD or the probing GET.
    Probe,
    /// Segment data transfer.
    Transfer,
}

/// A transport or local failure, already reduced from `reqwest`/`io` error sources (by
/// walking `source()`, never by string matching).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferErrorKind {
    /// Connect timed out.
    ConnectTimeout,
    /// Connection refused (also feeds [`HostState::on_refused_at_open`]).
    ConnectRefused,
    /// Peer reset or aborted the connection.
    ConnectionReset,
    /// Read stall: no data within the read timeout.
    ReadTimeout,
    /// The body ended before its declared length.
    PrematureEof,
    /// Temporary DNS failure.
    DnsTemporary,
    /// Permanent DNS failure (name does not exist).
    DnsFailure,
    /// TLS or certificate failure.
    Tls,
    /// Disk full (Win32 112 / 39).
    DiskFull,
    /// Destination device or folder gone.
    DeviceGone,
    /// Any other local I/O error.
    LocalIo,
    /// Redirect hop limit exceeded.
    TooManyRedirects,
    /// Redirect loop.
    RedirectLoop,
    /// https to http redirect.
    RedirectDowngrade,
    /// Redirect to a blocked destination or scheme.
    RedirectBlocked,
    /// Malformed HTTP framing (duplicate or conflicting lengths, bad status line, ...).
    MalformedFraming,
}

/// Classifies an HTTP error status (ARCHITECTURE 4.10). `retry_after` is the parsed
/// `Retry-After`, used for 429 and 503 only. For a 3xx `Retry-After` call
/// [`retry_after_disposition`] directly.
#[must_use]
pub fn classify_status(
    status: u16,
    phase: Phase,
    retry_after: Option<Duration>,
    now: Instant,
) -> Disposition {
    match status {
        408 | 500 | 502 | 504 => Disposition::Retry,
        429 | 503 => retry_after.map_or(Disposition::Cooldown, |delay| {
            retry_after_disposition(delay, now)
        }),
        401 | 407 => Disposition::NeedsCredentials,
        403 | 404 | 410 if phase == Phase::Transfer => Disposition::NeedsUrlRefresh,
        _ => Disposition::Fatal(DownloadErrorCode::HttpStatus),
    }
}

/// Classifies a transport or local failure (ARCHITECTURE 4.10).
#[must_use]
pub const fn classify_transfer(kind: TransferErrorKind) -> Disposition {
    use DownloadErrorCode as Code;
    use TransferErrorKind as K;
    match kind {
        K::ConnectTimeout
        | K::ConnectRefused
        | K::ConnectionReset
        | K::ReadTimeout
        | K::PrematureEof
        | K::DnsTemporary => Disposition::Retry,
        K::DnsFailure => Disposition::Fatal(Code::DnsFailure),
        K::Tls => Disposition::Fatal(Code::TlsFailure),
        K::DiskFull => Disposition::PauseDiskFull,
        K::DeviceGone => Disposition::PauseDestinationUnavailable,
        K::LocalIo => Disposition::Fatal(Code::DiskIo),
        K::TooManyRedirects => Disposition::Fatal(Code::TooManyRedirects),
        K::RedirectLoop => Disposition::Fatal(Code::RedirectLoop),
        K::RedirectDowngrade => Disposition::Fatal(Code::RedirectDowngrade),
        K::RedirectBlocked => Disposition::Fatal(Code::RedirectBlocked),
        K::MalformedFraming => Disposition::Fatal(Code::RangeProtocolViolation),
    }
}

// ---------------------------------------------------------------------------------------
// Per-host state
// ---------------------------------------------------------------------------------------

/// Per-host throttling state: a cooldown deadline and an AIMD connection limit (A.9: halve
/// on 429/503/refused-at-open, minimum 1; add one back after 2 minutes clean).
#[derive(Debug, Clone)]
pub struct HostState {
    max_connections: u32,
    limit: u32,
    cooldown_until: Option<Instant>,
    clean_since: Option<Instant>,
}

impl HostState {
    /// A host allowed up to `max_connections` (at least 1) concurrent connections.
    #[must_use]
    pub fn new(max_connections: u32) -> Self {
        let max = max_connections.max(1);
        Self {
            max_connections: max,
            limit: max,
            cooldown_until: None,
            clean_since: None,
        }
    }

    /// The current connection limit.
    #[must_use]
    pub const fn connection_limit(&self) -> u32 {
        self.limit
    }

    /// Whether the host is cooling down at `now`.
    #[must_use]
    pub fn is_blocked(&self, now: Instant) -> bool {
        self.cooldown_until.is_some_and(|until| now < until)
    }

    /// Time left of the cooldown at `now`.
    #[must_use]
    pub fn cooldown_remaining(&self, now: Instant) -> Duration {
        self.cooldown_until
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(now))
    }

    /// 429/503: halve the limit and cool the host down for `cooldown`, clamped to
    /// [`RETRY_AFTER_WAIT_MAX`] so a hostile `Retry-After` cannot overflow the clock. An existing longer
    /// cooldown is kept.
    pub fn on_throttled(&mut self, now: Instant, cooldown: Duration) {
        let until = now + cooldown.min(RETRY_AFTER_WAIT_MAX);
        self.cooldown_until = Some(self.cooldown_until.map_or(until, |old| old.max(until)));
        self.halve(now);
    }

    /// Another 429/503 while already cooling down: keep the longer cooldown without halving
    /// the limit again (one burst of simultaneous throttles is one reduction).
    pub fn extend_cooldown(&mut self, now: Instant, cooldown: Duration) {
        let until = now + cooldown.min(RETRY_AFTER_WAIT_MAX);
        self.cooldown_until = Some(self.cooldown_until.map_or(until, |old| old.max(until)));
    }

    /// A connection was refused at open: halve the limit without a cooldown.
    pub fn on_refused_at_open(&mut self, now: Instant) {
        self.halve(now);
    }

    /// Adds back one connection per full clean interval elapsed since the last reduction.
    pub fn recover(&mut self, now: Instant) {
        let Some(since) = self.clean_since else {
            return;
        };
        let intervals =
            now.saturating_duration_since(since).as_secs() / AIMD_RECOVERY_INTERVAL.as_secs();
        let steps = u32::try_from(intervals).unwrap_or(u32::MAX);
        if steps == 0 {
            return;
        }
        self.limit = self.limit.saturating_add(steps).min(self.max_connections);
        self.clean_since =
            (self.limit < self.max_connections).then(|| since + AIMD_RECOVERY_INTERVAL * steps);
    }

    fn halve(&mut self, now: Instant) {
        self.limit = (self.limit / 2).max(1);
        self.clean_since = Some(now);
    }
}
