//! S1.3 progress estimator (ARCHITECTURE 4.12). Pure: every sample carries its own instant.

use std::time::Duration;

use forgedm_core::progress::{Estimator, SpeedMeter, TICK, apply_eta_hysteresis};
use tokio::time::Instant;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Drives an estimator with a steady byte stream in 250 ms ticks, tracking time and bytes.
struct Feed {
    t0: Instant,
    tick: u64,
    bytes: u64,
}

impl Feed {
    fn new(t0: Instant) -> Self {
        Self {
            t0,
            tick: 0,
            bytes: 0,
        }
    }

    /// Feeds `ticks` ticks at `rate` bytes/s (zero for a stall). Returns the byte counter.
    fn run(&mut self, est: &mut Estimator, ticks: u64, rate: u64) -> u64 {
        for _ in 0..ticks {
            self.tick += 1;
            self.bytes += rate / 4;
            est.sample(self.t0 + ms(250 * self.tick), self.bytes);
        }
        self.bytes
    }
}

fn close(actual: f64, expected: f64, rel: f64) -> bool {
    (actual - expected).abs() <= expected * rel
}

#[test]
fn tick_is_250ms() {
    assert_eq!(TICK, ms(250));
}

#[test]
fn estimator_converges() {
    let t0 = Instant::now();
    let mut est = Estimator::new(t0, Some(10 * 1024 * MIB), 0);
    let mut feed = Feed::new(t0);
    feed.run(&mut est, 240, MIB); // 60 s
    let rate = MIB as f64;
    assert!(close(est.speed(), rate, 0.01), "display {}", est.speed());
    assert!(
        close(est.eta_speed(), rate, 0.01),
        "eta {}",
        est.eta_speed()
    );

    // A step to 4x converges: the display (tau 3 s) well before the ETA (tau 12 s).
    feed.run(&mut est, 40, 4 * MIB); // 10 s
    assert!(
        est.speed() > 3.8 * rate,
        "display after 10 s: {}",
        est.speed()
    );
    assert!(
        est.eta_speed() < est.speed(),
        "ETA speed lags the display speed"
    );
    feed.run(&mut est, 400, 4 * MIB); // 100 s
    assert!(
        close(est.eta_speed(), 4.0 * rate, 0.02),
        "{}",
        est.eta_speed()
    );
}

#[test]
fn stall_decays_to_zero() {
    let t0 = Instant::now();
    let mut est = Estimator::new(t0, Some(1024 * MIB), 0);
    let mut feed = Feed::new(t0);
    feed.run(&mut est, 40, MIB); // 10 s steady
    assert!(est.speed() > 0.9 * MIB as f64);
    let mut last = est.speed();
    for _ in 0..240 {
        feed.run(&mut est, 1, 0);
        assert!(est.speed() <= last, "monotone decay");
        last = est.speed();
    }
    assert!(est.speed() < 1.0, "after 60 s of stall: {}", est.speed());
    // The ETA speed is smoothed with tau 12 s (twice), so it decays slower than the display
    // speed; once it falls below 1 KiB/s the ETA disappears.
    feed.run(&mut est, 4 * 180, 0);
    assert!(est.eta_speed() < 1024.0, "{}", est.eta_speed());
    assert_eq!(est.eta(), None, "no ETA after a long stall");
}

#[test]
fn startup_bias_normalized() {
    let t0 = Instant::now();
    let mut est = Estimator::new(t0, None, 0);
    // One 250 ms sample at 1 MiB/s. An un-normalised EWMA starting at zero would report ~8%.
    est.sample(t0 + ms(250), 256 * KIB);
    assert!(close(est.speed(), MIB as f64, 0.01), "{}", est.speed());
    est.sample(t0 + ms(500), 512 * KIB);
    assert!(close(est.speed(), MIB as f64, 0.01), "{}", est.speed());
    assert!(
        close(est.eta_speed(), MIB as f64, 0.01),
        "{}",
        est.eta_speed()
    );

    // Non-uniform sample spacing keeps the normalisation exact.
    let mut est = Estimator::new(t0, None, 0);
    est.sample(t0 + ms(125), 128 * KIB);
    est.sample(t0 + ms(1125), 1152 * KIB);
    est.sample(t0 + ms(1250), 1280 * KIB);
    assert!(close(est.speed(), MIB as f64, 0.01), "{}", est.speed());
    assert!(
        close(est.eta_speed(), MIB as f64, 0.01),
        "{}",
        est.eta_speed()
    );
}

#[test]
fn eta_none_when_unknown_or_warmup() {
    let t0 = Instant::now();

    // Unknown total: never an ETA.
    let mut unknown = Estimator::new(t0, None, 0);
    Feed::new(t0).run(&mut unknown, 80, MIB);
    assert_eq!(unknown.eta(), None);

    // Known total: none before 2 s of elapsed time.
    let mut est = Estimator::new(t0, Some(100 * MIB), 0);
    let mut feed = Feed::new(t0);
    feed.run(&mut est, 7, MIB); // 1.75 s
    assert_eq!(est.eta(), None, "warm-up");
    feed.run(&mut est, 1, MIB); // 2.0 s
    assert!(est.eta().is_some(), "after 2 s with speed >= 1 KiB/s");

    // Below 1 KiB/s: none.
    let mut slow = Estimator::new(t0, Some(100 * MIB), 0);
    Feed::new(t0).run(&mut slow, 80, 512);
    assert_eq!(slow.eta(), None, "slower than 1 KiB/s");

    // A total that appears later enables the ETA.
    let mut late = Estimator::new(t0, None, 0);
    let mut feed = Feed::new(t0);
    feed.run(&mut late, 40, MIB);
    assert_eq!(late.eta(), None);
    late.set_total(Some(100 * MIB));
    feed.run(&mut late, 1, MIB);
    assert!(late.eta().is_some());

    // Fully received: zero remaining.
    let mut done = Estimator::new(t0, Some(10 * MIB), 0);
    Feed::new(t0).run(&mut done, 40, MIB);
    assert_eq!(done.eta(), Some(Duration::ZERO));
}

#[test]
fn eta_uses_min_of_speed_and_rate_limit() {
    let t0 = Instant::now();
    let mut est = Estimator::new(t0, Some(1024 * MIB), 0);
    let mut feed = Feed::new(t0);
    feed.run(&mut est, 80, MIB);
    let free = est.eta().unwrap();
    // A limit below the observed speed is the ETA speed.
    est.set_rate_limit(Some(MIB / 2));
    feed.run(&mut est, 1, MIB);
    let limited = est.eta().unwrap();
    assert!(limited > free + s(500), "{free:?} vs {limited:?}");
    // A limit above the observed speed changes nothing.
    est.set_rate_limit(Some(100 * MIB));
    feed.run(&mut est, 1, MIB);
    let unlimited = est.eta().unwrap();
    assert!(unlimited < limited, "{unlimited:?} vs {limited:?}");
}

#[test]
fn eta_hysteresis() {
    // Pure rule: keep the shown value unless the new one differs by more than 5% or 2 s.
    let hold = |shown: u64, raw_ms: u64| apply_eta_hysteresis(Some(s(shown)), ms(raw_ms));
    assert_eq!(hold(100, 101_500), s(100), "1.5 s and 1.5%");
    assert_eq!(hold(100, 98_500), s(100));
    assert_eq!(hold(100, 102_000), s(100), "exactly 2 s holds");
    assert_eq!(hold(100, 102_100), ms(102_100), "beyond 2 s changes");
    assert_eq!(hold(10, 10_400), s(10), "4% holds");
    assert_eq!(hold(10, 10_600), ms(10_600), "beyond 5% changes");
    assert_eq!(hold(10, 9_400), ms(9_400));
    assert_eq!(
        apply_eta_hysteresis(None, ms(55_555)),
        ms(55_555),
        "first value shown as is"
    );

    // Through the estimator: a steady stream counts down in steps, not every 250 ms tick.
    let t0 = Instant::now();
    let mut est = Estimator::new(t0, Some(10_000 * MIB), 0);
    let mut feed = Feed::new(t0);
    feed.run(&mut est, 40, MIB);
    let mut shown = vec![est.eta().unwrap()];
    for _ in 0..100 {
        feed.run(&mut est, 1, MIB);
        let eta = est.eta().unwrap();
        if Some(&eta) != shown.last() {
            shown.push(eta);
        }
    }
    assert!(
        shown.len() <= 14,
        "hysteresis limits updates: {} changes in 25 s",
        shown.len()
    );
    assert!(shown.len() >= 5, "but it still follows the countdown");
    assert!(
        shown.windows(2).all(|w| w[1] < w[0]),
        "steady stream only counts down"
    );
}

#[test]
fn worker_speed_tau_2_5s() {
    let t0 = Instant::now();
    let mut meter = SpeedMeter::worker(t0, 0);
    assert_eq!(meter.speed(), 0.0);
    // Normalised from the first sample.
    meter.sample(t0 + ms(250), 128 * KIB);
    assert!(
        close(meter.speed(), 512.0 * KIB as f64, 0.01),
        "{}",
        meter.speed()
    );
    for k in 2..=80 {
        meter.sample(t0 + ms(250 * k), k * 128 * KIB);
    }
    assert!(close(meter.speed(), 512.0 * KIB as f64, 0.01));
    // Stall: decays with tau 2.5 s, so about e^-4 (1.8%) after 10 s.
    for k in 81..=120 {
        meter.sample(t0 + ms(250 * k), 80 * 128 * KIB);
    }
    let ratio = meter.speed() / (512.0 * KIB as f64);
    assert!((0.012..0.025).contains(&ratio), "{ratio}");
}

#[test]
fn zero_dt_and_regressing_counters_are_safe() {
    let t0 = Instant::now();
    let mut est = Estimator::new(t0, Some(MIB), 100);
    est.sample(t0, 100);
    est.sample(t0 + ms(250), 50); // counter went backwards: treated as no progress
    assert!(est.speed().is_finite() && est.speed() >= 0.0);
    est.sample(t0 + ms(250), 60); // same instant again
    assert!(est.speed().is_finite() && est.speed() >= 0.0);
}

#[test]
fn regressed_counter_does_not_double_count_recovered_bytes() {
    let t0 = Instant::now();
    let mut est = Estimator::new(t0, Some(MIB), 100);
    est.sample(t0 + ms(250), 100);
    let before = est.speed();
    est.sample(t0 + ms(500), 50);
    est.sample(t0 + ms(750), 100);
    assert!(
        est.speed() <= before,
        "regained bytes must not count as fresh progress: {} > {before}",
        est.speed()
    );
}
