//! Adversarial flag self-tests (S1.4), driven over real loopback sockets.

mod common;

use common::{Client, get, host_port};
use forgedm_fixture::{FixtureServer, content};
use std::time::{Duration, Instant};

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

async fn server() -> String {
    FixtureServer::start().await.expect("start").0
}

#[tokio::test]
async fn bad_cr_variants_emitted() {
    let base = server().await;
    let cases = [
        ("start", "bytes 101-199/300000"),
        ("end", "bytes 100-200/300000"),
        ("total", "bytes 100-199/300001"),
        ("syntax", "bytes garbage"),
        ("inverted", "bytes 200-100/300000"),
        ("ge_total", "bytes 300000-300099/300000"),
        (
            "overflow",
            "bytes 100-99999999999999999999999/99999999999999999999999",
        ),
    ];
    for (variant, want) in cases {
        let r = get(
            &base,
            &path(&format!("?bad_cr={variant}")),
            &[("Range", "bytes=100-199")],
        )
        .await;
        assert_eq!(r.status, 206, "{variant}");
        assert_eq!(r.header("content-range"), Some(want), "{variant}");
        assert_eq!(r.body, expected(100, 100), "{variant} body stays valid");
    }
    let r = get(&base, &path("?bad_cr=start"), &[]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-range"), None);
    assert_eq!(get(&base, &path("?bad_cr=nope"), &[]).await.status, 400);
}

#[tokio::test]
async fn short206_returns_fewer_bytes_with_consistent_headers() {
    let base = server().await;
    let r = get(&base, &path("?short206=30"), &[("Range", "bytes=100-199")]).await;
    assert_eq!(r.status, 206);
    assert_eq!(r.header("content-range"), Some("bytes 100-169/300000"));
    assert_eq!(r.header("content-length"), Some("70"));
    assert_eq!(r.body, expected(100, 70));
    let r = get(&base, &path("?short206=500"), &[("Range", "bytes=100-199")]).await;
    assert_eq!(r.header("content-range"), Some("bytes 100-100/300000"));
    assert_eq!(r.body, expected(100, 1));
}

#[tokio::test]
async fn stall_after_stops_without_closing() {
    let base = server().await;
    let mut c = Client::connect(&base).await;
    let head = c
        .send_head_only("GET", &path("?stall_after=1000"), &[])
        .await;
    assert_eq!(head.status, 200);
    assert_eq!(c.read_n(1000).await, expected(0, 1000));
    assert!(
        c.idle_for(Duration::from_millis(500)).await,
        "no more bytes and no close"
    );
}

#[tokio::test]
async fn mutable_etag_if_range_returns_200() {
    let base = server().await;
    let flags = "?etag=mutable&mutate_after=2";
    let first = get(&base, &path(flags), &[]).await;
    let etag = first.header("etag").expect("etag").to_owned();
    assert_eq!(first.body, expected(0, SIZE));
    let ranged = [("Range", "bytes=0-9"), ("If-Range", etag.as_str())];
    let second = get(&base, &path(flags), &ranged).await;
    assert_eq!(second.status, 206, "still unmutated at request index 1");
    let third = get(&base, &path(flags), &ranged).await;
    assert_eq!(third.status, 200, "mutated: If-Range no longer matches");
    assert_ne!(third.header("etag"), Some(etag.as_str()));
    assert_eq!(third.body.len(), 300_000);
    assert_ne!(third.body, expected(0, SIZE), "content changed too");
}

#[tokio::test]
async fn lm_eq_date_matches_date_and_defeats_if_range() {
    let base = server().await;
    let r = get(&base, &path("?lm_eq_date=1&etag=none"), &[]).await;
    let lm = r.header("last-modified").expect("lm").to_owned();
    assert_eq!(r.header("date"), Some(lm.as_str()));
    assert!(lm.ends_with(" GMT"));
    let r = get(
        &base,
        &path("?lm_eq_date=1&etag=none"),
        &[("Range", "bytes=0-9"), ("If-Range", lm.as_str())],
    )
    .await;
    assert_eq!(r.status, 200, "a Last-Modified equal to Date is not strong");
}

#[tokio::test]
async fn status_codes_emitted_with_retry_after() {
    let base = server().await;
    for code in [401, 403, 404, 410, 416, 429, 500, 503] {
        let r = get(&base, &path(&format!("?status={code}")), &[]).await;
        assert_eq!(r.status, code);
        assert_eq!(r.header("retry-after"), None);
    }
    let r = get(&base, &path("?status=429&retry_after=120"), &[]).await;
    assert_eq!(r.header("retry-after"), Some("120"));
    let r = get(
        &base,
        &path("?status=503&retry_after=Sun,%2006%20Nov%201994%2008:49:37%20GMT"),
        &[],
    )
    .await;
    assert_eq!(
        r.header("retry-after"),
        Some("Sun, 06 Nov 1994 08:49:37 GMT")
    );
    let r = get(&base, &path("?status=416"), &[]).await;
    assert_eq!(r.header("content-range"), Some("bytes */300000"));
    let r = get(&base, &path("?status=401"), &[]).await;
    assert!(r.header("www-authenticate").is_some());
    assert_eq!(get(&base, &path("?status=418"), &[]).await.status, 400);
    assert_eq!(get(&base, &path("?retry_after=1"), &[]).await.status, 400);
}

#[tokio::test]
async fn fail_first_then_succeeds() {
    let base = server().await;
    let flags = path("?status=503&fail_first=2&retry_after=1");
    assert_eq!(get(&base, &flags, &[]).await.status, 503);
    assert_eq!(get(&base, &flags, &[]).await.status, 503);
    let ok = get(&base, &flags, &[]).await;
    assert_eq!(ok.status, 200);
    assert_eq!(ok.body, expected(0, SIZE));
}

#[tokio::test]
async fn max_conns_503_past_limit() {
    let base = server().await;
    let slow = "/f/3/1000000?max_conns=2&slow=1000";
    let mut a = Client::connect(&base).await;
    let mut b = Client::connect(&base).await;
    assert_eq!(a.send_head_only("GET", slow, &[]).await.status, 200);
    assert_eq!(b.send_head_only("GET", slow, &[]).await.status, 200);
    let third = get(&base, "/f/3/1000?max_conns=2", &[]).await;
    assert_eq!(third.status, 503);
    drop((a, b));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if get(&base, "/f/3/1000?max_conns=2", &[]).await.status == 200 {
            break;
        }
        assert!(Instant::now() < deadline, "slots never released");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn multipart_byteranges_emitted() {
    let base = server().await;
    let r = get(
        &base,
        &path("?multipart=1"),
        &[("Range", "bytes=0-9,100-109")],
    )
    .await;
    assert_eq!(r.status, 206);
    let ct = r.header("content-type").expect("ct");
    let boundary = ct
        .strip_prefix("multipart/byteranges; boundary=")
        .expect("multipart type");
    assert_eq!(
        r.header("content-length"),
        Some(r.body.len().to_string().as_str())
    );
    let text = String::from_utf8_lossy(&r.body).into_owned();
    assert!(text.contains(&format!("--{boundary}\r\n")));
    assert!(text.contains("Content-Range: bytes 0-9/300000"));
    assert!(text.contains("Content-Range: bytes 100-109/300000"));
    assert!(text.ends_with(&format!("--{boundary}--\r\n")));
    let part = expected(100, 10);
    assert!(r.body.windows(10).any(|w| w == part.as_slice()));
    // A single range is also answered as multipart.
    let r = get(&base, &path("?multipart=1"), &[("Range", "bytes=0-9")]).await;
    assert_eq!(r.status, 206);
    assert!(
        r.header("content-type")
            .is_some_and(|v| v.starts_with("multipart/byteranges"))
    );
}

#[tokio::test]
async fn range_200_after_ignores_ranges_later() {
    let base = server().await;
    let flags = path("?range_200_after=1");
    let ranged = [("Range", "bytes=0-9")];
    assert_eq!(
        get(&base, &flags, &[]).await.status,
        200,
        "plain GETs do not count"
    );
    assert_eq!(get(&base, &flags, &ranged).await.status, 206);
    let r = get(&base, &flags, &ranged).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body.len(), 300_000);
}

#[tokio::test]
async fn slow_from_throttles_only_after_offset() {
    let base = server().await;
    let mut c = Client::connect(&base).await;
    let started = Instant::now();
    let head = c
        .send_head_only("GET", "/f/7/25000?slow=5000&slow_from=20000", &[])
        .await;
    assert_eq!(head.status, 200);
    let fast = c.read_n(20_000).await;
    assert!(started.elapsed() < Duration::from_secs(3), "prefix is fast");
    let mut want = vec![0u8; 20_000];
    content::fill(7, 0, &mut want);
    assert_eq!(fast, want);
    let rest = c.read_n(5_000).await;
    assert!(
        started.elapsed() >= Duration::from_millis(700),
        "tail is throttled"
    );
    assert_eq!(rest.len(), 5_000);
    assert_eq!(
        get(&base, "/f/7/100?slow_from=5", &[]).await.status,
        400,
        "slow_from needs slow"
    );
}

#[tokio::test]
async fn redirect_cross_private_and_downgrade_locations() {
    let base = server().await;
    let port = host_port(&base)
        .rsplit(':')
        .next()
        .expect("port")
        .to_owned();
    let cases = [
        ("cross", format!("http://cross.test:{port}/f/11/300000")),
        ("private", format!("http://192.168.0.10:{port}/f/11/300000")),
        ("downgrade", format!("http://127.0.0.1:{port}/f/11/300000")),
    ];
    for (mode, want) in cases {
        let r = get(&base, &path(&format!("?redirect={mode}&t=k")), &[]).await;
        assert_eq!(r.status, 302, "{mode}");
        let loc = r.header("location").expect("location");
        assert_eq!(loc, format!("{want}?t=k"), "{mode}");
    }
}

#[tokio::test]
async fn redirect_loop_points_to_itself() {
    let base = server().await;
    let target = path("?redirect=loop&t=x");
    let r = get(&base, &target, &[]).await;
    assert_eq!(r.status, 302);
    assert_eq!(r.header("location"), Some(target.as_str()));
}

#[tokio::test]
async fn redirect_chain_counts_down() {
    let base = server().await;
    let r = get(&base, &path("?redirect=chain:2"), &[]).await;
    assert_eq!(r.status, 302);
    assert_eq!(
        r.header("location"),
        Some(path("?redirect=chain:1").as_str())
    );
    let r = get(&base, &path("?redirect=chain:1"), &[]).await;
    assert_eq!(
        r.header("location"),
        Some(path("?redirect=chain:0").as_str())
    );
    let r = get(&base, &path("?redirect=chain:0"), &[]).await;
    assert_eq!(r.status, 200);
    assert_eq!(
        get(&base, &path("?redirect=chain:x"), &[]).await.status,
        400
    );
}

#[tokio::test]
async fn redirect_signed_expiring_403_after_ttl() {
    let base = server().await;
    let start = path("?redirect=signed_expiring&ttl_requests=2");
    let r = get(&base, &start, &[]).await;
    assert_eq!(r.status, 302);
    let loc = r.header("location").expect("location").to_owned();
    assert!(loc.contains("signed="), "{loc}");
    assert_eq!(get(&base, &loc, &[]).await.status, 200);
    assert_eq!(get(&base, &loc, &[]).await.status, 200);
    assert_eq!(get(&base, &loc, &[]).await.status, 403, "link expired");
    let again = get(&base, &start, &[]).await;
    let fresh = again.header("location").expect("location").to_owned();
    assert_ne!(fresh, loc, "a new redirect issues a new token");
    assert_eq!(get(&base, &fresh, &[]).await.status, 200);
}

#[tokio::test]
async fn auth_basic_requires_authorization() {
    let base = server().await;
    let r = get(&base, &path("?auth=basic"), &[]).await;
    assert_eq!(r.status, 401);
    assert!(
        r.header("www-authenticate")
            .is_some_and(|v| v.starts_with("Basic"))
    );
    let r = get(
        &base,
        &path("?auth=basic"),
        &[("Authorization", "Basic Zm9vOmJhcg==")],
    )
    .await;
    assert_eq!(r.status, 200);
    assert_eq!(get(&base, &path("?auth=digest"), &[]).await.status, 400);
}

#[tokio::test]
async fn encoding_gzip_on_200_and_206() {
    let base = server().await;
    let r = get(&base, &path("?encoding=gzip"), &[]).await;
    assert_eq!(
        (r.status, r.header("content-encoding")),
        (200, Some("gzip"))
    );
    let r = get(&base, &path("?encoding=gzip"), &[("Range", "bytes=0-9")]).await;
    assert_eq!(
        (r.status, r.header("content-encoding")),
        (206, Some("gzip"))
    );
    assert_eq!(r.body, expected(0, 10));
}

#[tokio::test]
async fn zero_byte_size_serves_empty_200_and_416_ranges() {
    let base = server().await;
    let r = get(&base, "/f/5/0", &[]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-length"), Some("0"));
    assert!(r.body.is_empty() && r.complete);
    let r = get(&base, "/f/5/0", &[("Range", "bytes=0-")]).await;
    assert_eq!(r.status, 416);
    assert_eq!(r.header("content-range"), Some("bytes */0"));
    let r = get(&base, "/f/5/0?cl=0", &[]).await;
    assert_eq!(r.status, 200);
    assert!(r.body.is_empty() && r.complete);
}

#[tokio::test]
async fn bad_cr_with_huge_size_does_not_panic() {
    let base = server().await;
    let huge = u64::MAX;
    for variant in ["total", "ge_total", "start", "end", "inverted"] {
        let target = format!("/f/1/{huge}?bad_cr={variant}");
        let r = get(&base, &target, &[("Range", "bytes=0-9")]).await;
        assert_eq!(r.status, 206, "{variant}");
        assert!(r.header("content-range").is_some(), "{variant}");
    }
    assert_eq!(
        get(&base, &path(""), &[]).await.status,
        200,
        "still serving"
    );
}

#[tokio::test]
async fn multipart_overlapping_ranges_on_huge_size_do_not_panic() {
    let base = server().await;
    let target = format!("/f/1/{}?multipart=1", u64::MAX);
    let mut c = Client::connect(&base).await;
    let head = c
        .send_head_only("GET", &target, &[("Range", "bytes=0-,0-")])
        .await;
    assert!(
        head.status == 200 || head.status == 206,
        "a response arrives instead of a dropped connection: {}",
        head.status
    );
    drop(c);
    assert_eq!(
        get(&base, &path(""), &[]).await.status,
        200,
        "still serving"
    );
}

#[tokio::test]
async fn ttl_requests_zero_forces_an_expired_signed_link() {
    let base = server().await;
    let r = get(&base, &path("?signed=tok&ttl_requests=0"), &[]).await;
    assert_eq!(r.status, 403);
}
