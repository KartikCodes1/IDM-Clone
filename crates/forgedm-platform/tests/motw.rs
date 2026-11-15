//! Mark-of-the-Web tests (PLAN S1.6). The IAttachmentExecute backend is faked except in the
//! single `#[ignore]` test that talks to the real shell service (it depends on antivirus).

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers outside #[test] fns

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forgedm_platform::PlatformError;
use forgedm_platform::motw::{
    AttachmentExecutor, IaeFailure, IaeRequest, MotwOutcome, MotwService, ZoneInfo,
    iae_failure_from_hresult, write_zone_identifier, zone_source_url,
};

const ABOUT_INTERNET: &str = "about:internet";
const SHORT_TIMEOUT: Duration = Duration::from_millis(300);
const LONG_TIMEOUT: Duration = Duration::from_secs(30);

fn read_ads(path: &Path) -> String {
    let mut stream = path.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    fs::read_to_string(PathBuf::from(stream)).expect("read Zone.Identifier stream")
}

fn has_ads(path: &Path) -> bool {
    let mut stream = path.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    fs::read(PathBuf::from(stream)).is_ok()
}

fn info(host: &str) -> ZoneInfo {
    ZoneInfo {
        zone_id: 3,
        host_url: host.to_owned(),
        referrer_url: None,
    }
}

fn file_with(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write test file");
    path
}

/// Fake executor that records calls and returns a scripted result.
struct FakeExecutor {
    calls: Arc<Mutex<Vec<IaeRequest>>>,
    result: fn(&IaeRequest) -> Result<(), IaeFailure>,
}

impl AttachmentExecutor for FakeExecutor {
    fn apply(&self, request: &IaeRequest) -> Result<(), IaeFailure> {
        self.calls.lock().unwrap().push(request.clone());
        (self.result)(request)
    }
}

fn fake(
    result: fn(&IaeRequest) -> Result<(), IaeFailure>,
) -> (FakeExecutor, Arc<Mutex<Vec<IaeRequest>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        FakeExecutor {
            calls: Arc::clone(&calls),
            result,
        },
        calls,
    )
}

#[test]
fn sanitizer_strips_query_fragment_userinfo() {
    let url = "https://user:pass@Example.COM:8443/a/b.zip?token=SECRET&x=1#frag";
    let out = zone_source_url(url, false).unwrap();
    assert_eq!(out, "https://example.com:8443/a/b.zip");
    assert!(!out.contains("SECRET") && !out.contains("pass") && !out.contains("frag"));
    assert_eq!(
        zone_source_url("http://host.test", false).unwrap(),
        "http://host.test/"
    );
    assert_eq!(
        zone_source_url("http://host.test/?q=1", false).unwrap(),
        "http://host.test/"
    );
}

#[test]
fn record_full_keeps_query_but_never_userinfo() {
    let out = zone_source_url("https://u:p@host.test/f?a=1#z", true).unwrap();
    assert_eq!(out, "https://host.test/f?a=1#z");
}

#[test]
fn non_http_or_overlong_about_internet() {
    for url in [
        "ftp://host.test/f",
        "file:///C:/secret.txt",
        "javascript:alert(1)",
        "data:text/plain,hi",
        "",
        "host.test/no-scheme",
        "http://",
        "https:///path-without-host",
        "http://bad host/x",
    ] {
        assert_eq!(
            zone_source_url(url, false).unwrap(),
            ABOUT_INTERNET,
            "{url}"
        );
    }
    let long = format!("https://host.test/{}", "a".repeat(5000));
    assert_eq!(zone_source_url(&long, false).unwrap(), ABOUT_INTERNET);
    assert_eq!(zone_source_url(&long, true).unwrap(), ABOUT_INTERNET);
}

#[test]
fn crlf_rejected() {
    for url in [
        "https://host.test/a\r\nHostUrl=evil",
        "https://host.test/a\nb",
        "https://host.test/a\rb",
        "ftp://host.test/a\r\n",
    ] {
        assert!(
            matches!(
                zone_source_url(url, false),
                Err(PlatformError::InvalidInput(_))
            ),
            "{url:?}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "a.bin", b"x");
    let mut bad = info("https://host.test/ok");
    bad.referrer_url = Some("https://r.test/\r\nZoneId=0".to_owned());
    assert!(matches!(
        write_zone_identifier(&file, &bad),
        Err(PlatformError::InvalidInput(_))
    ));
    assert!(!has_ads(&file), "nothing may be written on rejection");
}

#[test]
fn direct_ads_readback_zoneid3_hosturl_without_query() {
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "a.bin", b"payload");
    let host = zone_source_url("https://h.test/dl/a.bin?sig=TOPSECRET#f", false).unwrap();
    let referrer = zone_source_url("https://h.test/page?id=9", false).unwrap();
    let zone = ZoneInfo {
        zone_id: 3,
        host_url: host,
        referrer_url: Some(referrer),
    };

    write_zone_identifier(&file, &zone).unwrap();

    let text = read_ads(&file);
    assert_eq!(
        text,
        "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://h.test/page\r\nHostUrl=https://h.test/dl/a.bin\r\n"
    );
    assert!(!text.contains("TOPSECRET") && !text.contains('?') && !text.contains('#'));
    assert_eq!(
        fs::read(&file).unwrap(),
        b"payload",
        "main stream untouched"
    );
}

#[test]
fn direct_ads_omits_referrer_when_none_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "a.bin", b"x");
    write_zone_identifier(&file, &info("https://h.test/a")).unwrap();
    write_zone_identifier(&file, &info("https://h.test/a")).unwrap();
    assert_eq!(
        read_ads(&file),
        "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://h.test/a\r\n"
    );
}

#[test]
fn direct_write_refuses_missing_file_instead_of_creating_it() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent.bin");
    assert!(write_zone_identifier(&missing, &info("https://h.test/a")).is_err());
    assert!(!missing.exists(), "the base file must not be created");
}

#[test]
fn direct_write_works_on_path_over_260_chars() {
    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_path_buf();
    for _ in 0..4 {
        deep.push("d".repeat(70));
    }
    let verbatim = forgedm_platform::wide::verbatim_path(&deep).unwrap();
    fs::create_dir_all(&verbatim).unwrap();
    let file = verbatim.join("long.bin");
    fs::write(&file, b"x").unwrap();
    assert!(file.as_os_str().len() > 260);

    write_zone_identifier(&file, &info("https://h.test/a")).unwrap();

    assert!(read_ads(&file).contains("ZoneId=3"));
}

#[test]
fn zero_byte_uses_direct_write() {
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "empty.bin", b"");
    let (exec, calls) = fake(|_| Ok(()));
    let service = MotwService::with_executor(exec, LONG_TIMEOUT).unwrap();

    let outcome = service.protect(&file, &info("https://h.test/e")).unwrap();

    assert_eq!(outcome, MotwOutcome::MarkedDirect);
    assert!(
        calls.lock().unwrap().is_empty(),
        "IAE must not see empty files"
    );
    assert!(file.exists(), "empty file must survive");
    assert!(read_ads(&file).contains("HostUrl=https://h.test/e"));
}

#[test]
fn iae_success_reports_marked_and_passes_sanitised_source() {
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "a.bin", b"data");
    let (exec, calls) = fake(|_| Ok(()));
    let service = MotwService::with_executor(exec, LONG_TIMEOUT).unwrap();
    let mut zone = info("https://h.test/a");
    zone.referrer_url = Some("https://h.test/r".to_owned());

    let outcome = service.protect(&file, &zone).unwrap();

    assert_eq!(outcome, MotwOutcome::Marked);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].source, "https://h.test/a");
    assert_eq!(calls[0].referrer.as_deref(), Some("https://h.test/r"));
    assert!(calls[0].path.ends_with("a.bin"));
}

#[test]
fn iae_policy_block_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "a.bin", b"data");
    let (exec, _) = fake(|_| Err(IaeFailure::BlockedByPolicy));
    let service = MotwService::with_executor(exec, LONG_TIMEOUT).unwrap();

    let outcome = service.protect(&file, &info("https://h.test/a")).unwrap();

    assert_eq!(outcome, MotwOutcome::BlockedByPolicy);
}

#[test]
fn iae_failure_with_file_removed_reports_antivirus() {
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "a.bin", b"data");
    let (exec, _) = fake(|req| {
        fs::remove_file(&req.path).unwrap();
        Err(IaeFailure::Failed(0x8000_4005))
    });
    let service = MotwService::with_executor(exec, LONG_TIMEOUT).unwrap();

    let outcome = service.protect(&file, &info("https://h.test/a")).unwrap();

    assert_eq!(outcome, MotwOutcome::RemovedByAntivirus);
}

#[test]
fn iae_failure_with_file_present_falls_back_to_direct() {
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "a.bin", b"data");
    let (exec, _) = fake(|_| Err(IaeFailure::Failed(0x8000_4005)));
    let service = MotwService::with_executor(exec, LONG_TIMEOUT).unwrap();

    let outcome = service.protect(&file, &info("https://h.test/a")).unwrap();

    assert_eq!(outcome, MotwOutcome::MarkedDirect);
    assert!(read_ads(&file).contains("ZoneId=3"));
}

#[test]
fn service_serial_and_clean_shutdown() {
    struct Tracking {
        active: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
        total: Arc<AtomicUsize>,
    }
    impl AttachmentExecutor for Tracking {
        fn apply(&self, _: &IaeRequest) -> Result<(), IaeFailure> {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(15));
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.total.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let active = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let total = Arc::new(AtomicUsize::new(0));
    let exec = Tracking {
        active: Arc::clone(&active),
        max_seen: Arc::clone(&max_seen),
        total: Arc::clone(&total),
    };
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(MotwService::with_executor(exec, LONG_TIMEOUT).unwrap());

    let workers: Vec<_> = (0..6)
        .map(|i| {
            let service = Arc::clone(&service);
            let file = file_with(dir.path(), &format!("f{i}.bin"), b"data");
            std::thread::spawn(move || service.protect(&file, &info("https://h.test/a")).unwrap())
        })
        .collect();
    for worker in workers {
        assert_eq!(worker.join().unwrap(), MotwOutcome::Marked);
    }
    assert_eq!(total.load(Ordering::SeqCst), 6);
    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        1,
        "IAE calls must be serial"
    );

    drop(service);

    // The worker owned the executor (holding three Arc clones); after a clean shutdown the
    // thread has exited and released them, leaving only the test's own handles.
    assert_eq!(
        Arc::strong_count(&active),
        1,
        "worker thread must have exited"
    );
}

#[test]
fn iae_timeout_poisons_worker_falls_back_to_direct() {
    struct Hanging {
        release: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }
    impl AttachmentExecutor for Hanging {
        fn apply(&self, _: &IaeRequest) -> Result<(), IaeFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(20);
            while !self.release.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        }
    }
    let release = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let exec = Hanging {
        release: Arc::clone(&release),
        calls: Arc::clone(&calls),
    };
    let dir = tempfile::tempdir().unwrap();
    let service = MotwService::with_executor(exec, SHORT_TIMEOUT).unwrap();
    let first = file_with(dir.path(), "first.bin", b"data");
    let second = file_with(dir.path(), "second.bin", b"data");

    let started = Instant::now();
    let outcome = service.protect(&first, &info("https://h.test/1")).unwrap();
    assert_eq!(outcome, MotwOutcome::MarkedDirect);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(service.is_poisoned());
    assert!(read_ads(&first).contains("HostUrl=https://h.test/1"));

    // While poisoned, later items go straight to the direct write without waiting.
    let started = Instant::now();
    let outcome = service.protect(&second, &info("https://h.test/2")).unwrap();
    assert_eq!(outcome, MotwOutcome::MarkedDirect);
    assert!(
        started.elapsed() < SHORT_TIMEOUT,
        "poisoned path must not wait"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no new IAE call while poisoned"
    );
    assert!(read_ads(&second).contains("HostUrl=https://h.test/2"));

    // Once the stuck call returns the worker recovers and IAE is used again.
    release.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(10);
    while service.is_poisoned() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!service.is_poisoned(), "worker must recover");
    let third = file_with(dir.path(), "third.bin", b"data");
    let outcome = service.protect(&third, &info("https://h.test/3")).unwrap();
    assert_eq!(outcome, MotwOutcome::Marked);
}

#[test]
#[ignore = "talks to the real shell attachment service; depends on antivirus"]
fn iae_from_sta_writes_zone_identifier() {
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "real.bin", b"real attachment payload");
    let service = MotwService::start().unwrap();

    let outcome = service
        .protect(&file, &info("https://h.test/real.bin"))
        .unwrap();

    assert_eq!(outcome, MotwOutcome::Marked, "IAE must do the marking");
    let text = read_ads(&file);
    assert!(text.contains("ZoneId=3"), "{text}");
    assert!(!text.contains('?'), "{text}");
}

#[test]
fn security_problem_hresult_is_blocked_from_check_policy_and_save() {
    // INET_E_SECURITY_PROBLEM = 0x800C000E (Urlmon.h), as an i32 HRESULT.
    let hr = i32::from_ne_bytes(0x800C_000E_u32.to_ne_bytes());
    assert_eq!(
        iae_failure_from_hresult(hr, true),
        IaeFailure::BlockedByPolicy
    );
    assert_eq!(
        iae_failure_from_hresult(hr, false),
        IaeFailure::BlockedByPolicy
    );
    // E_ACCESSDENIED
    let denied = i32::from_ne_bytes(0x8007_0005_u32.to_ne_bytes());
    assert_eq!(
        iae_failure_from_hresult(denied, false),
        IaeFailure::BlockedByPolicy
    );
}

#[test]
fn other_check_policy_failure_is_blocked_but_other_save_failure_is_not() {
    let hr = i32::from_ne_bytes(0x8000_4005_u32.to_ne_bytes()); // E_FAIL
    assert_eq!(
        iae_failure_from_hresult(hr, true),
        IaeFailure::BlockedByPolicy
    );
    assert_eq!(
        iae_failure_from_hresult(hr, false),
        IaeFailure::Failed(0x8000_4005)
    );
}

#[test]
fn unicode_line_separators_rejected() {
    for bad in ["https://h.test/a\u{2028}b", "https://h.test/a\u{2029}b"] {
        assert!(
            matches!(
                zone_source_url(bad, false),
                Err(PlatformError::InvalidInput(_))
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn timeout_with_file_gone_is_not_attributed_to_antivirus() {
    struct RemoveThenHang;
    impl AttachmentExecutor for RemoveThenHang {
        fn apply(&self, request: &IaeRequest) -> Result<(), IaeFailure> {
            let _ = fs::remove_file(&request.path);
            std::thread::sleep(Duration::from_millis(900));
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "gone.bin", b"data");
    let service = MotwService::with_executor(RemoveThenHang, Duration::from_millis(200)).unwrap();
    let result = service.protect(&file, &info("https://h.test/x"));
    assert!(matches!(result, Err(PlatformError::Io(_))), "{result:?}");
}

/// Regression: a queued job whose caller timed out right after the worker went idle used to
/// leave the poison flag set forever. Timing based, so it runs many trials; with the fix the
/// worker always recovers.
#[test]
fn skipping_abandoned_job_never_leaves_worker_poisoned() {
    struct FirstCallSleeps {
        calls: AtomicUsize,
        sleep: Duration,
    }
    impl AttachmentExecutor for FirstCallSleeps {
        fn apply(&self, _: &IaeRequest) -> Result<(), IaeFailure> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                std::thread::sleep(self.sleep);
            }
            Ok(())
        }
    }
    let timeout = Duration::from_millis(15);
    let dir = tempfile::tempdir().unwrap();
    let file = file_with(dir.path(), "race.bin", b"data");
    for trial in 0..400_u64 {
        let exec = FirstCallSleeps {
            calls: AtomicUsize::new(0),
            sleep: timeout,
        };
        let service = Arc::new(MotwService::with_executor(exec, timeout).unwrap());
        let first = {
            let (service, file) = (Arc::clone(&service), file.clone());
            std::thread::spawn(move || service.protect(&file, &info("https://h.test/a")))
        };
        // Spin (sleep is too coarse) so the second deadline sweeps across the first job's end.
        let spin_until = Instant::now() + Duration::from_micros(trial * 2);
        while Instant::now() < spin_until {
            std::hint::spin_loop();
        }
        let second = {
            let (service, file) = (Arc::clone(&service), file.clone());
            std::thread::spawn(move || service.protect(&file, &info("https://h.test/b")))
        };
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while service.is_poisoned() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!service.is_poisoned(), "stuck poisoned in trial {trial}");
    }
}
