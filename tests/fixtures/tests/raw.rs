//! Raw TCP modes (wire violations hyper will not emit) and request-log extensions (S1.4).

mod common;

use common::{Client, get, raw_exchange, split_head};
use forgedm_fixture::{FixtureServer, RequestLog, content};
use std::time::Duration;

async fn server() -> (String, RequestLog) {
    FixtureServer::start().await.expect("start")
}

fn request(target: &str) -> String {
    format!("GET {target} HTTP/1.1\r\nHost: fixture\r\n\r\n")
}

fn count_header(head: &str, name: &str) -> usize {
    head.lines()
        .filter(|l| l.to_ascii_lowercase().starts_with(&format!("{name}:")))
        .count()
}

#[tokio::test]
async fn raw_dup_cl_emitted() {
    let (base, _log) = server().await;
    let raw = raw_exchange(&base, &request("/f/1/1000?raw=dup_cl")).await;
    let (head, body) = split_head(&raw);
    assert!(head.starts_with("HTTP/1.1 200"));
    assert_eq!(count_header(&head, "content-length"), 2);
    assert!(head.contains("Content-Length: 1000") && head.contains("Content-Length: 1001"));
    assert_eq!(body.len(), 1000);
}

#[tokio::test]
async fn raw_cl_and_te_emitted() {
    let (base, _log) = server().await;
    let raw = raw_exchange(&base, &request("/f/1/1000?raw=cl_and_te")).await;
    let (head, body) = split_head(&raw);
    assert_eq!(count_header(&head, "content-length"), 1);
    assert_eq!(count_header(&head, "transfer-encoding"), 1);
    assert!(
        body.starts_with(b"3e8\r\n"),
        "chunk size line for 1000 bytes"
    );
    assert!(body.ends_with(b"0\r\n\r\n"));
}

#[tokio::test]
async fn raw_bad_status_emitted() {
    let (base, _log) = server().await;
    let raw = raw_exchange(&base, &request("/f/1/10?raw=bad_status")).await;
    assert!(
        raw.starts_with(b"HTTP/1.1 2x0 "),
        "{:?}",
        String::from_utf8_lossy(&raw)
    );
}

#[tokio::test]
async fn raw_huge_headers_emitted() {
    let (base, _log) = server().await;
    let raw = raw_exchange(&base, &request("/f/1/10?raw=huge_headers")).await;
    let (head, _body) = split_head(&raw);
    assert!(head.len() > 128 * 1024, "head is {} bytes", head.len());
    assert!(head.lines().count() > 128, "more than 128 header lines");
}

#[tokio::test]
async fn raw_crlf_location_emitted() {
    let (base, _log) = server().await;
    let raw = raw_exchange(&base, &request("/f/1/10?raw=crlf_location")).await;
    let has = |needle: &[u8]| raw.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"HTTP/1.1 302"));
    assert!(has(b"\nX-Injected: 1"), "injected line present");
    assert!(!has(b"\r\nX-Injected"), "the injection uses a bare LF");
}

#[tokio::test]
async fn raw_lie_cl_over_and_under() {
    let (base, _log) = server().await;
    let raw = raw_exchange(&base, &request("/f/1/1000?lie_cl=+50")).await;
    let (head, body) = split_head(&raw);
    assert!(head.starts_with("HTTP/1.1 200") && head.contains("Content-Length: 1050"));
    assert_eq!(body.len(), 1000, "50 bytes short of the claim");
    let raw = raw_exchange(&base, &request("/f/1/1000?lie_cl=-50")).await;
    let (head, body) = split_head(&raw);
    assert!(head.contains("Content-Length: 950"));
    assert_eq!(body.len(), 1000, "50 bytes beyond the claim");
    let mut want = vec![0u8; 1000];
    content::fill(1, 0, &mut want);
    assert_eq!(body, want);
}

#[tokio::test]
async fn raw_unknown_mode_is_400_and_logged() {
    let (base, log) = server().await;
    let raw = raw_exchange(&base, &request("/f/1/10?raw=nope&t=bad")).await;
    assert!(raw.starts_with(b"HTTP/1.1 400"));
    assert_eq!(log.for_test("bad").len(), 1);
}

#[tokio::test]
async fn raw_mode_on_reused_connection_is_400() {
    let (base, _log) = server().await;
    let mut c = Client::connect(&base).await;
    assert_eq!(c.send("GET", "/f/1/10", &[]).await.status, 200);
    assert_eq!(c.send("GET", "/f/1/10?raw=dup_cl", &[]).await.status, 400);
}

#[tokio::test]
async fn log_tracks_max_concurrency() {
    let (base, log) = server().await;
    assert_eq!(log.max_concurrency(), 0);
    get(&base, "/f/1/10", &[]).await;
    assert_eq!(log.max_concurrency(), 1);
    let slow = "/f/3/1000000?slow=1000";
    let mut a = Client::connect(&base).await;
    let mut b = Client::connect(&base).await;
    a.send_head_only("GET", slow, &[]).await;
    b.send_head_only("GET", slow, &[]).await;
    assert_eq!(log.max_concurrency(), 2);
    drop((a, b));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while log.current_concurrency() != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    get(&base, "/f/1/10", &[]).await;
    assert_eq!(log.max_concurrency(), 2, "high-water mark never decreases");
    assert_eq!(log.current_concurrency(), 0, "slots released");
}

#[tokio::test]
async fn log_records_peer_port_and_header_presence_not_values() {
    let (base, log) = server().await;
    let mut c = Client::connect(&base).await;
    let port = c.local_port();
    c.send(
        "GET",
        "/f/1/10",
        &[
            ("Authorization", "Bearer s3cret-token"),
            ("Cookie", "sid=c00kie"),
        ],
    )
    .await;
    c.send("GET", "/f/1/10", &[]).await;
    let all = log.entries();
    assert_eq!(all[0].peer_port, port);
    assert_eq!(all[1].peer_port, port);
    assert!(all[0].has_authorization && all[0].has_cookie);
    assert!(!all[1].has_authorization && !all[1].has_cookie);
    let dump = format!("{all:?}");
    assert!(!dump.contains("s3cret") && !dump.contains("c00kie"));
}

#[tokio::test]
async fn lie_cl_rejects_flags_it_cannot_honour() {
    let (base, _log) = server().await;
    for flag in [
        "slow=1000",
        "drop_after=5",
        "stall_after=5",
        "max_conns=1",
        "status=500",
        "redirect=chain:1",
        "signed=x",
        "auth=basic",
        "multipart=1",
    ] {
        let raw = raw_exchange(&base, &request(&format!("/f/1/100?lie_cl=+5&{flag}"))).await;
        assert!(raw.starts_with(b"HTTP/1.1 400"), "{flag}");
    }
}

#[tokio::test]
async fn lie_cl_head_honours_head_flag() {
    let (base, _log) = server().await;
    let head = |q: &str| format!("HEAD /f/1/100?{q} HTTP/1.1\r\nHost: fixture\r\n\r\n");
    let raw = raw_exchange(&base, &head("lie_cl=+5&head=0")).await;
    assert!(
        raw.starts_with(b"HTTP/1.1 405"),
        "{:?}",
        String::from_utf8_lossy(&raw)
    );
    let raw = raw_exchange(&base, &head("lie_cl=+5")).await;
    assert!(raw.starts_with(b"HTTP/1.1 200"));
}

#[tokio::test]
async fn raw_malformed_request_gets_400() {
    let (base, _log) = server().await;
    let bad = "GET /f/1/10?raw=dup_cl HTTP/1.1\r\nno-colon-here\r\n\r\n";
    let raw = raw_exchange(&base, bad).await;
    assert!(
        raw.starts_with(b"HTTP/1.1 400"),
        "{:?}",
        String::from_utf8_lossy(&raw)
    );
}
