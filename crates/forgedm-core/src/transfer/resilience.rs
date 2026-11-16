//! What a failed attempt turns into (ARCHITECTURE 4.10, ADR-014): the retry loops of the
//! probe, the single-stream transfer and every segment worker all ask [`Resilience`].
//!
//! One instance serves one running download. It owns the failure budgets (8 consecutive
//! no-progress failures per segment, 20 per download), the backoff jitter, the limited
//! retries for connect-level failures, the one-per-failure URL re-resolve, and a handle on
//! the shared per-host cooldown. Waiting goes through the injected [`Sleeper`] and is cut
//! short by the caller's cancellation token, so pause and cancel are immediate.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use forgedm_protocol::{DownloadErrorCode, DownloadErrorDetails};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::clock::{Cancelled, Sleeper, TokioSleeper, sleep_or_cancel};
use crate::engine::hosts::HostHandle;
use crate::error::{Failure, Fault, TaskStop};
use crate::ports::unix_ms;
use crate::retry::{
    Disposition, Jitter, Phase, RETRY_BASE, ReresolveLimiter, RetryBudget, RetryPolicy,
    TransferErrorKind, classify_status, classify_transfer,
};

/// Retries allowed for failures that say the server may not be reachable at all (connect
/// timeout or refusal, DNS): three retries, then the specific error is final.
pub const LIMITED_RETRIES: u32 = 3;
/// Shortest cooldown applied to a host that throttled without a usable `Retry-After`.
const COOLDOWN_MIN: Duration = RETRY_BASE;

/// How retries wait and jitter. Tests inject a recording sleeper and a seed.
#[derive(Clone)]
pub struct RetryEnv {
    /// Where waits go.
    pub sleeper: Arc<dyn Sleeper>,
    /// Seed of the backoff jitter; `None` seeds from the process's random keys.
    pub jitter_seed: Option<u64>,
}

impl Default for RetryEnv {
    fn default() -> Self {
        Self {
            sleeper: Arc::new(TokioSleeper),
            jitter_seed: None,
        }
    }
}

impl std::fmt::Debug for RetryEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryEnv")
            .field("jitter_seed", &self.jitter_seed)
            .finish_non_exhaustive()
    }
}

/// What the caller's retry loop does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Try the request again (the wait, if any, already happened).
    Retry,
    /// The caller's token fired during the wait.
    Cancelled,
    /// Stop with this outcome.
    Stop(TaskStop),
}

#[derive(Debug)]
struct State {
    download: RetryBudget,
    segments: HashMap<u32, RetryBudget>,
    limited: u32,
    reresolved: HashSet<u32>,
    reresolve: ReresolveLimiter,
}

/// Retry decisions and waits of one running download.
pub struct Resilience {
    sleeper: Arc<dyn Sleeper>,
    policy: RetryPolicy,
    jitter: Mutex<Jitter>,
    host: Arc<HostHandle>,
    state: Mutex<State>,
}

impl std::fmt::Debug for Resilience {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resilience").finish_non_exhaustive()
    }
}

/// The error code a fault stands for, recorded as `last` of an exhausted budget.
fn code_of(fault: Fault) -> DownloadErrorCode {
    match fault {
        Fault::Status {
            status: 429 | 503, ..
        } => DownloadErrorCode::RateLimited,
        other => match other.into_stop() {
            TaskStop::Fail(failure) => failure.code,
            _ => DownloadErrorCode::Internal,
        },
    }
}

const fn is_limited(fault: Fault) -> bool {
    use TransferErrorKind as K;
    matches!(
        fault,
        Fault::Transport(K::ConnectTimeout | K::ConnectRefused | K::DnsTemporary | K::DnsFailure)
    )
}

impl Resilience {
    /// A fresh set of budgets over the shared `host` state.
    #[must_use]
    pub fn new(env: &RetryEnv, host: Arc<HostHandle>) -> Self {
        let jitter = env
            .jitter_seed
            .map_or_else(Jitter::from_entropy, Jitter::seeded);
        Self {
            sleeper: Arc::clone(&env.sleeper),
            policy: RetryPolicy::default(),
            jitter: Mutex::new(jitter),
            host,
            state: Mutex::new(State {
                download: RetryBudget::per_download(),
                segments: HashMap::new(),
                limited: 0,
                reresolved: HashSet::new(),
                reresolve: ReresolveLimiter::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn jittered(&self, attempt: u32) -> Duration {
        let mut jitter = self.jitter.lock().unwrap_or_else(PoisonError::into_inner);
        self.policy.delay(attempt, &mut jitter)
    }

    /// The connection limit the host currently allows (AIMD).
    #[must_use]
    pub fn host_limit(&self) -> u32 {
        self.host.limit(Instant::now())
    }

    /// Waits out the host's cooldown, if it is cooling down. Every request is preceded by
    /// this, so one 429 holds back all connections to the host.
    ///
    /// # Errors
    /// [`Cancelled`] when `cancel` fires first.
    pub async fn gate(&self, cancel: &CancellationToken) -> Result<(), Cancelled> {
        let remaining = self.host.cooldown_remaining(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        sleep_or_cancel(self.sleeper.as_ref(), remaining, cancel).await
    }

    /// Records `bytes` of progress for `scope` (a segment number, 0 for a single stream).
    pub fn on_progress(&self, scope: u32, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let now = Instant::now();
        let mut state = self.lock();
        state.download.on_progress(bytes, now);
        if let Some(budget) = state.segments.get_mut(&scope) {
            budget.on_progress(bytes, now);
        }
        state.limited = 0;
        state.reresolved.remove(&scope);
    }

    /// Judges `fault`, which `scope` hit in `phase`, and waits as the verdict requires.
    pub async fn on_fault(
        &self,
        phase: Phase,
        scope: u32,
        fault: Fault,
        cancel: &CancellationToken,
    ) -> Verdict {
        let now = Instant::now();
        let disposition = match fault {
            // A name that does not resolve is retried a few times (resolvers hiccup).
            Fault::Transport(TransferErrorKind::DnsFailure) => Disposition::Retry,
            Fault::Transport(kind) => classify_transfer(kind),
            Fault::Status {
                status,
                retry_after,
            } => classify_status(status, phase, retry_after, now),
        };
        match disposition {
            Disposition::Retry => self.backoff(scope, fault, cancel).await,
            Disposition::RetryAfter { delay } => {
                self.host.throttled(now, delay);
                self.charge(scope, fault)
                    .map_or_else(Verdict::Stop, |_| Verdict::Retry)
            }
            Disposition::Cooldown => self.cooldown(scope, fault),
            Disposition::WaitUntil { at } => {
                let wait = at.saturating_duration_since(now);
                self.host.throttled(now, wait);
                let wait_ms = i64::try_from(wait.as_millis()).unwrap_or(0);
                Verdict::Stop(TaskStop::RetryWait {
                    at_ms: unix_ms().saturating_add(wait_ms),
                })
            }
            Disposition::NeedsCredentials => Verdict::Stop(TaskStop::NeedsCredentials),
            Disposition::NeedsUrlRefresh => self.reresolve(scope, now),
            Disposition::PauseDiskFull => {
                Verdict::Stop(TaskStop::PauseFor(DownloadErrorCode::DiskFull))
            }
            Disposition::PauseDestinationUnavailable => Verdict::Stop(TaskStop::PauseFor(
                DownloadErrorCode::DestinationUnavailable,
            )),
            Disposition::Fatal(DownloadErrorCode::RateLimited) => {
                Verdict::Stop(TaskStop::Fail(Failure::new(DownloadErrorCode::RateLimited)))
            }
            Disposition::Fatal(_) => Verdict::Stop(fault.into_stop()),
        }
    }

    /// Counts one failure against the budgets; returns the retry attempt number.
    fn charge(&self, scope: u32, fault: Fault) -> Result<u32, TaskStop> {
        let mut state = self.lock();
        if is_limited(fault) {
            state.limited += 1;
            if state.limited > LIMITED_RETRIES {
                return Err(fault.into_stop());
            }
        }
        let segment = state
            .segments
            .entry(scope)
            .or_insert_with(RetryBudget::per_segment)
            .on_failure();
        let download = state.download.on_failure();
        match (segment, download) {
            (Ok(a), Ok(b)) => Ok(a.max(b)),
            _ => Err(TaskStop::Fail(Failure {
                code: DownloadErrorCode::RetryBudgetExhausted,
                details: Some(DownloadErrorDetails {
                    last: Some(code_of(fault)),
                    ..DownloadErrorDetails::default()
                }),
            })),
        }
    }

    async fn backoff(&self, scope: u32, fault: Fault, cancel: &CancellationToken) -> Verdict {
        let attempt = match self.charge(scope, fault) {
            Ok(attempt) => attempt,
            Err(stop) => return Verdict::Stop(stop),
        };
        if fault == Fault::Transport(TransferErrorKind::ConnectRefused) {
            self.host.refused(Instant::now());
        }
        let delay = self.jittered(attempt);
        match sleep_or_cancel(self.sleeper.as_ref(), delay, cancel).await {
            Ok(()) => Verdict::Retry,
            Err(Cancelled) => Verdict::Cancelled,
        }
    }

    /// 429/503 without a usable `Retry-After`: cool the host down for a jittered backoff
    /// (at least one second); the next request of every connection waits it out in
    /// [`gate`](Self::gate).
    fn cooldown(&self, scope: u32, fault: Fault) -> Verdict {
        let attempt = match self.charge(scope, fault) {
            Ok(attempt) => attempt,
            Err(stop) => return Verdict::Stop(stop),
        };
        let delay = self.jittered(attempt).max(COOLDOWN_MIN);
        self.host.throttled(Instant::now(), delay);
        Verdict::Retry
    }

    /// 403/404/410 during transfer: one re-resolve (the next request starts again from the
    /// original URL and is judged against the stored identity), at most 3 per hour; the
    /// same failure again without progress ends in `needs_url_refresh`.
    fn reresolve(&self, scope: u32, now: Instant) -> Verdict {
        let mut state = self.lock();
        if state.reresolved.contains(&scope) || !state.reresolve.try_acquire(now) {
            return Verdict::Stop(TaskStop::NeedsUrlRefresh);
        }
        state.reresolved.insert(scope);
        Verdict::Retry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::BoxFuture;

    struct Recording(Mutex<Vec<Duration>>);

    impl Sleeper for Recording {
        fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
            self.0.lock().unwrap().push(duration);
            Box::pin(async {})
        }
    }

    fn setup() -> (Resilience, Arc<Recording>) {
        let sleeper = Arc::new(Recording(Mutex::new(Vec::new())));
        let env = RetryEnv {
            sleeper: sleeper.clone(),
            jitter_seed: Some(7),
        };
        let host = Arc::new(HostHandle::new(8));
        (Resilience::new(&env, host), sleeper)
    }

    const RESET: Fault = Fault::Transport(TransferErrorKind::ConnectionReset);

    #[tokio::test]
    async fn the_segment_budget_ends_a_run_of_failures_without_progress() {
        let (res, _) = setup();
        let cancel = CancellationToken::new();
        let mut retries = 0;
        let last = loop {
            match res.on_fault(Phase::Transfer, 1, RESET, &cancel).await {
                Verdict::Retry => retries += 1,
                other => break other,
            }
        };
        assert_eq!(
            retries, 7,
            "the 8th consecutive failure exhausts the budget"
        );
        let Verdict::Stop(TaskStop::Fail(failure)) = last else {
            unreachable!("expected a failure, got {last:?}");
        };
        assert_eq!(failure.code, DownloadErrorCode::RetryBudgetExhausted);
        assert_eq!(
            failure.details.and_then(|d| d.last),
            Some(DownloadErrorCode::NetworkReset)
        );
    }

    #[tokio::test]
    async fn dns_and_refusals_are_retried_a_limited_number_of_times() {
        let (res, sleeper) = setup();
        let cancel = CancellationToken::new();
        let dns = Fault::Transport(TransferErrorKind::DnsFailure);
        for _ in 0..LIMITED_RETRIES {
            assert_eq!(
                res.on_fault(Phase::Probe, 0, dns, &cancel).await,
                Verdict::Retry
            );
        }
        let last = res.on_fault(Phase::Probe, 0, dns, &cancel).await;
        let Verdict::Stop(TaskStop::Fail(failure)) = last else {
            unreachable!("expected a failure, got {last:?}");
        };
        assert_eq!(failure.code, DownloadErrorCode::DnsFailure);
        assert_eq!(sleeper.0.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn progress_resets_the_failure_count_and_the_re_resolve() {
        let (res, _) = setup();
        let cancel = CancellationToken::new();
        let gone = Fault::Status {
            status: 410,
            retry_after: None,
        };
        assert_eq!(
            res.on_fault(Phase::Transfer, 2, gone, &cancel).await,
            Verdict::Retry
        );
        assert_eq!(
            res.on_fault(Phase::Transfer, 2, gone, &cancel).await,
            Verdict::Stop(TaskStop::NeedsUrlRefresh)
        );
        res.on_progress(2, 10);
        assert_eq!(
            res.on_fault(Phase::Transfer, 2, gone, &cancel).await,
            Verdict::Retry
        );
    }

    #[tokio::test]
    async fn a_cancelled_wait_reports_cancelled_at_once() {
        let (res, _) = setup();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            res.on_fault(Phase::Transfer, 1, RESET, &cancel).await,
            Verdict::Cancelled
        );
    }
}
