//! S1.3 retry policy, classification, Retry-After, budgets and host state
//! (ARCHITECTURE 4.10, ADR-014; PHASE_0_1_PLAN A.7 and A.9).

use std::time::Duration;

use forgedm_core::retry::{
    BudgetExhausted, Disposition, HostState, Jitter, Phase, ReresolveLimiter, RetryBudget,
    RetryPolicy, TransferErrorKind, classify_status, classify_transfer, parse_retry_after,
    retry_after_disposition,
};
use forgedm_protocol::DownloadErrorCode as Code;
use tokio::time::Instant;

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn status_classification_table() {
    let now = Instant::now();
    let cls = |status, phase| classify_status(status, phase, None, now);
    let (p, t) = (Phase::Probe, Phase::Transfer);
    let fatal = Disposition::Fatal(Code::HttpStatus);
    for status in [408, 500, 502, 504] {
        assert_eq!(cls(status, t), Disposition::Retry, "{status} transfer");
        assert_eq!(cls(status, p), Disposition::Retry, "{status} probe");
    }
    for status in [429, 503] {
        assert_eq!(cls(status, t), Disposition::Cooldown, "{status}");
        assert_eq!(cls(status, p), Disposition::Cooldown, "{status}");
    }
    for status in [401, 407] {
        assert_eq!(cls(status, t), Disposition::NeedsCredentials, "{status}");
        assert_eq!(cls(status, p), Disposition::NeedsCredentials, "{status}");
    }
    for status in [403, 404, 410] {
        assert_eq!(cls(status, p), fatal, "{status} probe fails");
        assert_eq!(
            cls(status, t),
            Disposition::NeedsUrlRefresh,
            "{status} transfer"
        );
    }
    for status in [
        400, 405, 411, 414, 416, 418, 451, 499, 501, 505, 599, 200, 301,
    ] {
        assert_eq!(cls(status, t), fatal, "{status}");
        assert_eq!(cls(status, p), fatal, "{status}");
    }
}

#[test]
fn transfer_error_classification_table() {
    use TransferErrorKind as K;
    for kind in [
        K::ConnectTimeout,
        K::ConnectRefused,
        K::ConnectionReset,
        K::ReadTimeout,
        K::PrematureEof,
        K::DnsTemporary,
    ] {
        assert_eq!(classify_transfer(kind), Disposition::Retry, "{kind:?}");
    }
    let cases = [
        (K::DnsFailure, Disposition::Fatal(Code::DnsFailure)),
        (K::Tls, Disposition::Fatal(Code::TlsFailure)),
        (K::DiskFull, Disposition::PauseDiskFull),
        (K::DeviceGone, Disposition::PauseDestinationUnavailable),
        (K::LocalIo, Disposition::Fatal(Code::DiskIo)),
        (
            K::TooManyRedirects,
            Disposition::Fatal(Code::TooManyRedirects),
        ),
        (K::RedirectLoop, Disposition::Fatal(Code::RedirectLoop)),
        (
            K::RedirectDowngrade,
            Disposition::Fatal(Code::RedirectDowngrade),
        ),
        (
            K::RedirectBlocked,
            Disposition::Fatal(Code::RedirectBlocked),
        ),
        (
            K::MalformedFraming,
            Disposition::Fatal(Code::RangeProtocolViolation),
        ),
    ];
    for (kind, expected) in cases {
        assert_eq!(classify_transfer(kind), expected, "{kind:?}");
    }
}

#[test]
fn full_jitter_bounds_seeded() {
    let policy = RetryPolicy::default();
    let expected = [1, 2, 4, 8, 16, 32, 60, 60, 60, 60, 60];
    for (attempt, secs) in expected.iter().enumerate() {
        let attempt = u32::try_from(attempt).unwrap();
        assert_eq!(policy.ceiling(attempt), s(*secs), "ceiling {attempt}");
    }
    assert_eq!(policy.ceiling(u32::MAX), s(60));

    let mut jitter = Jitter::seeded(42);
    let mut lo = Duration::MAX;
    let mut hi = Duration::ZERO;
    for _ in 0..2000 {
        let d = policy.delay(5, &mut jitter);
        assert!(d <= s(32), "{d:?}");
        lo = lo.min(d);
        hi = hi.max(d);
    }
    assert!(lo < s(4), "full jitter reaches low values: {lo:?}");
    assert!(hi > s(28), "full jitter reaches the ceiling: {hi:?}");

    let draw = |seed| {
        let mut j = Jitter::seeded(seed);
        (0..16).map(|_| policy.delay(4, &mut j)).collect::<Vec<_>>()
    };
    assert_eq!(draw(7), draw(7), "same seed, same sequence");
    assert_ne!(draw(7), draw(8), "different seed, different sequence");
    let mut entropy = Jitter::from_entropy();
    assert!(policy.delay(0, &mut entropy) <= s(1));
}

#[test]
fn retry_after_seconds_and_http_date_vs_response_date() {
    let date = httpdate::parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
    assert_eq!(parse_retry_after("120", None), Some(s(120)));
    assert_eq!(parse_retry_after("  7 ", Some(date)), Some(s(7)));
    assert_eq!(parse_retry_after("0", None), Some(s(0)));
    assert_eq!(
        parse_retry_after("Wed, 21 Oct 2015 07:28:30 GMT", Some(date)),
        Some(s(30))
    );
    // A date in the past means "now".
    assert_eq!(
        parse_retry_after("Wed, 21 Oct 2015 07:27:00 GMT", Some(date)),
        Some(s(0))
    );
    // The HTTP-date form is relative to the response Date: without one it is unusable.
    assert_eq!(
        parse_retry_after("Wed, 21 Oct 2015 07:28:30 GMT", None),
        None
    );
}

#[test]
fn retry_after_clamped_and_beyond_cap_wait_until() {
    let now = Instant::now();
    assert_eq!(
        retry_after_disposition(s(0), now),
        Disposition::RetryAfter { delay: s(0) }
    );
    assert_eq!(
        retry_after_disposition(s(120), now),
        Disposition::RetryAfter { delay: s(120) }
    );
    assert_eq!(
        retry_after_disposition(s(121), now),
        Disposition::WaitUntil { at: now + s(121) }
    );
    assert_eq!(
        retry_after_disposition(s(3600), now),
        Disposition::WaitUntil { at: now + s(3600) }
    );
    assert_eq!(
        classify_status(503, Phase::Transfer, Some(s(30)), now),
        Disposition::RetryAfter { delay: s(30) }
    );
    assert_eq!(
        classify_status(429, Phase::Probe, Some(s(600)), now),
        Disposition::WaitUntil { at: now + s(600) }
    );
}

#[test]
fn retry_after_over_1h_is_rate_limited_failure() {
    let now = Instant::now();
    let fatal = Disposition::Fatal(Code::RateLimited);
    assert_eq!(retry_after_disposition(s(3601), now), fatal);
    assert_eq!(
        classify_status(429, Phase::Transfer, Some(s(86_400)), now),
        fatal
    );
    assert_eq!(
        parse_retry_after("99999999999999999999999", None),
        Some(Duration::MAX)
    );
    assert_eq!(retry_after_disposition(Duration::MAX, now), fatal);
}

#[test]
fn invalid_retry_after_ignored() {
    let date = httpdate::parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").ok();
    for bad in [
        "", " ", "abc", "-1", "+5", "1.5", "5s", "Wed 99", "1e3", "0x10", "\u{0663}",
    ] {
        assert_eq!(parse_retry_after(bad, date), None, "{bad:?}");
    }
    let now = Instant::now();
    assert_eq!(
        classify_status(429, Phase::Transfer, None, now),
        Disposition::Cooldown
    );
}

#[test]
fn budget_exhaust_and_reset_after_progress() {
    let t0 = Instant::now();
    let mut segment = RetryBudget::per_segment();
    for attempt in 0..7u32 {
        assert_eq!(segment.on_failure(), Ok(attempt));
    }
    assert_eq!(segment.on_failure(), Err(BudgetExhausted { limit: 8 }));

    // 1 MiB of progress resets the count.
    let mut segment = RetryBudget::per_segment();
    for _ in 0..5 {
        segment.on_failure().unwrap();
    }
    segment.on_progress(1024 * 1024 - 1, t0);
    assert_eq!(segment.failures(), 5, "just under 1 MiB does not reset");
    segment.on_progress(1, t0);
    assert_eq!(segment.failures(), 0);
    assert_eq!(segment.on_failure(), Ok(0));

    // 30 s of progress resets the count, measured from the first progress after a failure.
    let mut segment = RetryBudget::per_segment();
    segment.on_failure().unwrap();
    segment.on_progress(10, t0);
    segment.on_progress(10, t0 + s(29));
    assert_eq!(segment.failures(), 1);
    segment.on_progress(10, t0 + s(30));
    assert_eq!(segment.failures(), 0);

    // A failure restarts progress accounting.
    let mut segment = RetryBudget::per_segment();
    segment.on_failure().unwrap();
    segment.on_progress(1000, t0);
    segment.on_failure().unwrap();
    segment.on_progress(1000, t0 + s(40));
    assert_eq!(segment.failures(), 2);

    // The download budget is 20.
    let mut download = RetryBudget::per_download();
    for _ in 0..19 {
        download.on_failure().unwrap();
    }
    assert_eq!(download.on_failure(), Err(BudgetExhausted { limit: 20 }));
}

#[test]
fn host_cooldown_blocks_until_deadline() {
    let t0 = Instant::now();
    let mut host = HostState::new(8);
    assert!(!host.is_blocked(t0));
    host.on_throttled(t0, s(30));
    assert!(host.is_blocked(t0));
    assert!(host.is_blocked(t0 + s(29)));
    assert_eq!(host.cooldown_remaining(t0 + s(10)), s(20));
    assert!(!host.is_blocked(t0 + s(30)));
    assert_eq!(host.cooldown_remaining(t0 + s(31)), Duration::ZERO);

    // A shorter later cooldown never shortens an existing one.
    let mut host = HostState::new(8);
    host.on_throttled(t0, s(60));
    host.on_throttled(t0 + s(1), s(5));
    assert!(host.is_blocked(t0 + s(59)));
    assert!(!host.is_blocked(t0 + s(60)));
}

#[test]
fn aimd_halves_on_429_recovers() {
    let t0 = Instant::now();
    let mut host = HostState::new(8);
    assert_eq!(host.connection_limit(), 8);
    host.on_throttled(t0, s(1));
    assert_eq!(host.connection_limit(), 4);
    host.on_throttled(t0, s(1));
    host.on_refused_at_open(t0);
    assert_eq!(host.connection_limit(), 1);
    host.on_throttled(t0, s(1));
    assert_eq!(host.connection_limit(), 1, "never below 1");
    assert!(!host.is_blocked(t0 + s(2)));

    host.recover(t0 + s(119));
    assert_eq!(host.connection_limit(), 1);
    host.recover(t0 + s(120));
    assert_eq!(host.connection_limit(), 2);
    host.recover(t0 + s(120 + 119));
    assert_eq!(host.connection_limit(), 2);
    host.recover(t0 + s(120 * 3));
    assert_eq!(host.connection_limit(), 4, "elapsed intervals all count");
    host.recover(t0 + s(120 * 100));
    assert_eq!(
        host.connection_limit(),
        8,
        "capped at the configured maximum"
    );

    // A throttle restarts the clean-interval clock.
    let t1 = t0 + s(120 * 100);
    host.on_throttled(t1, s(1));
    assert_eq!(host.connection_limit(), 4);
    host.recover(t1 + s(119));
    assert_eq!(host.connection_limit(), 4);
    host.recover(t1 + s(120));
    assert_eq!(host.connection_limit(), 5);
}

#[test]
fn reresolve_limited_to_three_per_hour() {
    let t0 = Instant::now();
    let mut limiter = ReresolveLimiter::new();
    assert!(limiter.try_acquire(t0));
    assert!(limiter.try_acquire(t0 + s(10)));
    assert!(limiter.try_acquire(t0 + s(20)));
    assert!(!limiter.try_acquire(t0 + s(30)));
    assert!(!limiter.try_acquire(t0 + s(3599)));
    assert!(limiter.try_acquire(t0 + s(3600)), "the oldest use aged out");
    assert!(!limiter.try_acquire(t0 + s(3601)));
}

#[test]
fn on_throttled_with_oversized_cooldown_does_not_panic() {
    let t0 = Instant::now();
    let mut host = HostState::new(8);
    host.on_throttled(t0, Duration::MAX);
    host.on_throttled(t0, Duration::from_secs(u64::MAX));
    assert_eq!(host.connection_limit(), 2);
    assert!(host.is_blocked(t0 + s(3599)));
    assert!(host.cooldown_remaining(t0) <= Duration::from_secs(3600));
}
