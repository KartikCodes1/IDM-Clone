//! Progress, speed and ETA estimation (ARCHITECTURE 4.12).
//!
//! Pure and clock-free: every sample carries its own [`Instant`]. Speed is an exponentially
//! weighted moving average with time-based weights (`alpha = 1 - exp(-dt / tau)`), so uneven
//! sampling is handled correctly. Filters start at zero; the startup bias is removed by
//! running an identical filter over a constant input of 1 and dividing (exact for any
//! sample spacing). A stall feeds zero speed samples, so the estimate decays toward zero.

use std::time::Duration;

use tokio::time::Instant;

/// Sampling tick (A.9).
pub const TICK: Duration = Duration::from_millis(250);
/// Time constant of the displayed speed (A.9).
pub const DISPLAY_TAU: Duration = Duration::from_secs(3);
/// Time constant of each stage of the ETA speed (A.9, double-smoothed).
pub const ETA_TAU: Duration = Duration::from_secs(12);
/// Time constant of a per-worker speed (A.9).
pub const WORKER_TAU: Duration = Duration::from_millis(2500);
/// Minimum elapsed time before an ETA is shown (A.9).
pub const ETA_MIN_ELAPSED: Duration = Duration::from_secs(2);
/// Minimum smoothed speed in bytes per second before an ETA is shown (A.9: 1 KiB/s).
pub const ETA_MIN_SPEED: f64 = 1024.0;
/// A shown ETA only changes when the new value differs by more than this (A.9).
pub const ETA_HYSTERESIS_ABS: Duration = Duration::from_secs(2);
/// ... or by more than 1/`ETA_HYSTERESIS_REL_DIVISOR` of the shown value (A.9: 5%).
const ETA_HYSTERESIS_REL_DIVISOR: u32 = 20;
/// Speeds below this are reported as exactly zero (bytes per second).
const ZERO_SNAP: f64 = 0.5;

/// A bias-corrected time-based EWMA with `N` cascaded stages (`N = 2` is double smoothing).
#[derive(Debug, Clone)]
pub struct Ewma<const N: usize> {
    tau: f64,
    values: [f64; N],
    norms: [f64; N],
}

impl<const N: usize> Ewma<N> {
    /// A filter with time constant `tau`, initially at zero.
    #[must_use]
    pub fn new(tau: Duration) -> Self {
        Self {
            tau: tau.as_secs_f64().max(f64::MIN_POSITIVE),
            values: [0.0; N],
            norms: [0.0; N],
        }
    }

    /// Feeds `sample` as the input held over the last `dt`.
    pub fn update(&mut self, dt: Duration, sample: f64) {
        let alpha = 1.0 - (-dt.as_secs_f64() / self.tau).exp();
        let (mut input, mut norm_input) = (sample, 1.0);
        for (value, norm) in self.values.iter_mut().zip(self.norms.iter_mut()) {
            *value += alpha * (input - *value);
            *norm += alpha * (norm_input - *norm);
            (input, norm_input) = (*value, *norm);
        }
    }

    /// The bias-corrected estimate (zero before the first update).
    #[must_use]
    pub fn value(&self) -> f64 {
        match self.values.last().zip(self.norms.last()) {
            Some((&value, &norm)) if norm > 0.0 => {
                let estimate = value / norm;
                if estimate < ZERO_SNAP { 0.0 } else { estimate }
            }
            _ => 0.0,
        }
    }
}

/// Turns successive `(instant, cumulative bytes)` samples into `(dt, bytes/s)` pairs.
#[derive(Debug, Clone)]
struct Sampler {
    at: Instant,
    bytes: u64,
}

impl Sampler {
    /// Returns the interval and rate since the previous accepted sample, or `None` when no
    /// time has passed (the bytes then count toward the next interval). A counter that went
    /// backwards counts as no progress.
    fn advance(&mut self, now: Instant, bytes: u64) -> Option<(Duration, f64)> {
        let dt = now.saturating_duration_since(self.at);
        if dt.is_zero() {
            return None;
        }
        let delta = bytes.saturating_sub(self.bytes);
        self.at = now;
        self.bytes = self.bytes.max(bytes);
        Some((dt, delta as f64 / dt.as_secs_f64()))
    }
}

/// A single-stage speed meter (per-worker speeds feed split decisions).
#[derive(Debug, Clone)]
pub struct SpeedMeter {
    sampler: Sampler,
    ewma: Ewma<1>,
}

impl SpeedMeter {
    /// A meter with time constant `tau`, counting from `bytes` at `start`.
    #[must_use]
    pub fn new(tau: Duration, start: Instant, bytes: u64) -> Self {
        Self {
            sampler: Sampler { at: start, bytes },
            ewma: Ewma::new(tau),
        }
    }

    /// A per-worker meter (tau 2.5 s).
    #[must_use]
    pub fn worker(start: Instant, bytes: u64) -> Self {
        Self::new(WORKER_TAU, start, bytes)
    }

    /// Records the cumulative byte count at `now`.
    pub fn sample(&mut self, now: Instant, bytes: u64) {
        if let Some((dt, rate)) = self.sampler.advance(now, bytes) {
            self.ewma.update(dt, rate);
        }
    }

    /// Smoothed speed in bytes per second.
    #[must_use]
    pub fn speed(&self) -> f64 {
        self.ewma.value()
    }
}

/// Keeps `shown` unless `raw` differs from it by more than 2 s or 5% of `shown`.
#[must_use]
pub fn apply_eta_hysteresis(shown: Option<Duration>, raw: Duration) -> Duration {
    let Some(shown) = shown else {
        return raw;
    };
    let diff = raw.abs_diff(shown);
    let relative = shown / ETA_HYSTERESIS_REL_DIVISOR;
    if diff > ETA_HYSTERESIS_ABS || diff > relative {
        raw
    } else {
        shown
    }
}

/// Speed and ETA for one download.
///
/// Call [`Estimator::sample`] every [`TICK`] with the accepted-byte counter.
#[derive(Debug, Clone)]
pub struct Estimator {
    started: Instant,
    sampler: Sampler,
    bytes: u64,
    total: Option<u64>,
    rate_limit: Option<u64>,
    display: Ewma<1>,
    eta_filter: Ewma<2>,
    shown_eta: Option<Duration>,
}

impl Estimator {
    /// Starts at `started` with `initial_bytes` already accepted (for example after a
    /// restart) and an optional known `total`.
    #[must_use]
    pub fn new(started: Instant, total: Option<u64>, initial_bytes: u64) -> Self {
        Self {
            started,
            sampler: Sampler {
                at: started,
                bytes: initial_bytes,
            },
            bytes: initial_bytes,
            total,
            rate_limit: None,
            display: Ewma::new(DISPLAY_TAU),
            eta_filter: Ewma::new(ETA_TAU),
            shown_eta: None,
        }
    }

    /// Sets or clears the known total size.
    pub fn set_total(&mut self, total: Option<u64>) {
        self.total = total;
    }

    /// Sets or clears the effective rate limit in bytes per second; the ETA then uses
    /// `min(speed, limit)`.
    pub fn set_rate_limit(&mut self, limit: Option<u64>) {
        self.rate_limit = limit.filter(|&l| l > 0);
    }

    /// Records the cumulative accepted bytes at `now` and refreshes speed and ETA.
    pub fn sample(&mut self, now: Instant, bytes: u64) {
        self.bytes = self.bytes.max(bytes);
        if let Some((dt, rate)) = self.sampler.advance(now, bytes) {
            self.display.update(dt, rate);
            self.eta_filter.update(dt, rate);
        }
        self.shown_eta = self
            .raw_eta(now)
            .map(|raw| apply_eta_hysteresis(self.shown_eta, raw));
    }

    /// Display speed in bytes per second (tau 3 s).
    #[must_use]
    pub fn speed(&self) -> f64 {
        self.display.value()
    }

    /// Speed used for the ETA in bytes per second: double-smoothed (tau 12 s), capped by the
    /// rate limit when one is set.
    #[must_use]
    pub fn eta_speed(&self) -> f64 {
        let speed = self.eta_filter.value();
        self.rate_limit
            .map_or(speed, |limit| speed.min(limit as f64))
    }

    /// The ETA to display: `None` without a known total, before 2 s have elapsed, or below
    /// 1 KiB/s ("Estimating...").
    #[must_use]
    pub const fn eta(&self) -> Option<Duration> {
        self.shown_eta
    }

    fn raw_eta(&self, now: Instant) -> Option<Duration> {
        let total = self.total?;
        if now.saturating_duration_since(self.started) < ETA_MIN_ELAPSED {
            return None;
        }
        let speed = self.eta_speed();
        if speed < ETA_MIN_SPEED {
            return None;
        }
        let remaining = total.saturating_sub(self.bytes) as f64;
        Duration::try_from_secs_f64(remaining / speed).ok()
    }
}
