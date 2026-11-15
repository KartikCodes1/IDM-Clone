//! Server behaviour tests (S0.5), driven over real loopback sockets.

mod common;

use common::{Client, get, host_port};
use forgedm_fixture::{FixtureServer, content};

const SIZE: u64 = 300_000;
const SEED: u64 = 11;

fn expected(start: u64, len: u64) -> Vec<u8> {
    let mut v = vec![0u8; usize::try_from(len).expect("small")];
    content::fill(SEED, start, &mut v);
    v
}

fn path(flags: &str) -> String {
    format!("/f/{SEED}/{SIZE}{flags}")
}

#[tokio::test]
async fn full_200_body_matches_sha() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    let r = get(&base, &path(""), &[]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-length"), Some(SIZE.to_string().as_str()));
    assert_eq!(r.header("accept-ranges"), Some("bytes"));
    assert!(r.complete);
    let got = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&r.body);
        h.finalize().to_vec()
    };
    assert_eq!(got, content::sha256(SEED, SIZE).to_vec());
    assert_eq!(r.body, expected(0, SIZE));
}

#[tokio::test]
async fn single_range_206_valid_content_range() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    let r = get(&base, &path(""), &[("Range", "bytes=1000-1999")]).await;
    assert_eq!(r.status, 206);
    assert_eq!(r.header("content-range"), Some("bytes 1000-1999/300000"));
    assert_eq!(r.header("content-length"), Some("1000"));
    assert_eq!(r.body, expected(1000, 1000));
    // End beyond the size is clamped.
    let r = get(&base, &path(""), &[("Range", "bytes=299990-999999")]).await;
    assert_eq!(r.status, 206);
    assert_eq!(
        r.header("content-range"),
        Some("bytes 299990-299999/300000")
    );
    assert_eq!(r.body, expected(299_990, 10));
    // ranges=0 ignores Range and advertises nothing.
    let r = get(&base, &path("?ranges=0"), &[("Range", "bytes=0-9")]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("accept-ranges"), None);
    assert_eq!(r.body.len(), 300_000);
}

#[tokio::test]
async fn open_ended_and_suffix_ranges() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    let r = get(&base, &path(""), &[("Range", "bytes=299000-")]).await;
    assert_eq!(r.status, 206);
    assert_eq!(
        r.header("content-range"),
        Some("bytes 299000-299999/300000")
    );
    assert_eq!(r.body, expected(299_000, 1000));
    let r = get(&base, &path(""), &[("Range", "bytes=-500")]).await;
    assert_eq!(r.status, 206);
    assert_eq!(
        r.header("content-range"),
        Some("bytes 299500-299999/300000")
    );
    assert_eq!(r.body, expected(299_500, 500));
    // A suffix longer than the file is the whole file.
    let r = get(&base, &path(""), &[("Range", "bytes=-9999999")]).await;
    assert_eq!(r.status, 206);
    assert_eq!(r.header("content-range"), Some("bytes 0-299999/300000"));
}

#[tokio::test]
async fn unsatisfiable_416_star_total() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    for range in ["bytes=300000-", "bytes=300000-400000", "bytes=-0"] {
        let r = get(&base, &path(""), &[("Range", range)]).await;
        assert_eq!(r.status, 416, "{range}");
        assert_eq!(r.header("content-range"), Some("bytes */300000"), "{range}");
    }
    // Malformed range syntax is ignored (RFC 9110): full 200.
    let r = get(&base, &path(""), &[("Range", "bytes=5-2")]).await;
    assert_eq!(r.status, 200);
}

#[tokio::test]
async fn head_disabled_405() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    let r = Client::connect(&base)
        .await
        .send("HEAD", &path(""), &[])
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-length"), Some("300000"));
    assert!(r.body.is_empty());
    let r = Client::connect(&base)
        .await
        .send("HEAD", &path("?head=0"), &[])
        .await;
    assert_eq!(r.status, 405);
    let r = get(&base, &path("?head=0"), &[]).await;
    assert_eq!(r.status, 200, "GET still works when HEAD is disabled");
}

#[tokio::test]
async fn no_cl_is_chunked() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    let r = get(&base, &path("?cl=0"), &[]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-length"), None);
    assert_eq!(r.header("transfer-encoding"), Some("chunked"));
    assert!(r.complete);
    assert_eq!(r.body, expected(0, SIZE));
}

#[tokio::test]
async fn drop_after_closes_mid_body() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    for flags in ["?drop_after=1000", "?cl=0&drop_after=1000"] {
        let r = get(&base, &path(flags), &[]).await;
        assert_eq!(r.status, 200, "{flags}");
        assert!(!r.complete, "{flags}: body must be cut short");
        assert!(r.body.len() < 300_000, "{flags}");
        assert_eq!(
            r.body,
            expected(0, u64::try_from(r.body.len()).expect("small")),
            "{flags}: prefix is intact"
        );
    }
}

#[tokio::test]
async fn request_log_records_method_range_conn_id() {
    let (base, log) = FixtureServer::start().await.expect("start");
    let mut a = Client::connect(&base).await;
    a.send(
        "GET",
        &path("?t=alpha"),
        &[("Range", "bytes=0-9"), ("If-Range", "\"x\"")],
    )
    .await;
    a.send("HEAD", &path("?t=alpha"), &[]).await;
    let mut b = Client::connect(&base).await;
    b.send("GET", &path("?t=beta"), &[]).await;

    let all = log.entries();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].method, "GET");
    assert_eq!(all[0].range.as_deref(), Some("bytes=0-9"));
    assert_eq!(all[0].if_range.as_deref(), Some("\"x\""));
    assert_eq!(all[1].method, "HEAD");
    assert_eq!(all[1].range, None);
    assert_eq!(
        all[0].conn_id, all[1].conn_id,
        "keep-alive requests share a connection id"
    );
    assert_ne!(all[0].conn_id, all[2].conn_id);
    assert_eq!(log.for_test("alpha").len(), 2);
    assert_eq!(log.for_test("beta").len(), 1);
    assert!(all[0].target.contains("t=alpha"));
}

#[tokio::test]
async fn binds_loopback_only() {
    let server = FixtureServer::start_on(0).await.expect("start");
    assert!(server.addr().ip().is_loopback());
    assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
    assert!(server.base_url().starts_with("http://127.0.0.1:"));
    assert_eq!(host_port(&server.base_url()), server.addr().to_string());
    let (base, _log) = FixtureServer::start().await.expect("start");
    assert!(base.starts_with("http://127.0.0.1:"));
}

#[tokio::test]
async fn header_flags_etag_lm_cd_encoding_and_errors() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    let r = get(&base, &path(""), &[]).await;
    assert!(r.header("etag").is_some_and(|v| v.starts_with('"')));
    let r = get(&base, &path("?etag=weak"), &[]).await;
    assert!(r.header("etag").is_some_and(|v| v.starts_with("W/\"")));
    let r = get(&base, &path("?etag=none"), &[]).await;
    assert_eq!(r.header("etag"), None);
    let r = get(
        &base,
        &path("?lm=Sun,%2006%20Nov%201994%2008:49:37%20GMT"),
        &[],
    )
    .await;
    assert_eq!(
        r.header("last-modified"),
        Some("Sun, 06 Nov 1994 08:49:37 GMT")
    );
    let r = get(
        &base,
        &path("?cd=attachment%3B%20filename%3D%22a%20b.bin%22"),
        &[],
    )
    .await;
    assert_eq!(
        r.header("content-disposition"),
        Some("attachment; filename=\"a b.bin\"")
    );
    let r = get(&base, &path("?encoding=gzip"), &[]).await;
    assert_eq!(r.header("content-encoding"), Some("gzip"));
    assert_eq!(r.body, expected(0, SIZE), "encoding=gzip is a header only");
    assert_eq!(get(&base, "/nope", &[]).await.status, 404);
    assert_eq!(get(&base, "/f/abc/10", &[]).await.status, 400);
    assert_eq!(get(&base, &path("?slow=zero"), &[]).await.status, 400);
}

#[tokio::test]
async fn if_range_mismatch_returns_full_and_slow_throttles() {
    let (base, _log) = FixtureServer::start().await.expect("start");
    let first = get(&base, &path(""), &[]).await;
    let etag = first.header("etag").expect("etag").to_owned();
    let r = get(
        &base,
        &path(""),
        &[("Range", "bytes=0-9"), ("If-Range", &etag)],
    )
    .await;
    assert_eq!(r.status, 206);
    let r = get(
        &base,
        &path(""),
        &[("Range", "bytes=0-9"), ("If-Range", "\"other\"")],
    )
    .await;
    assert_eq!(r.status, 200);
    let started = std::time::Instant::now();
    let r = get(&base, "/f/1/4000?slow=10000", &[]).await;
    assert_eq!(r.body.len(), 4000);
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(250),
        "slow must throttle"
    );
}
