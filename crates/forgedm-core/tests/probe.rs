//! The probe (PLAN S1.9, ADR-009): an optional HEAD (5 s), then a mandatory
//! `GET Range: bytes=0-0`, against the fixture server's request log.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::time::{Duration, Instant};

use forgedm_core::client::{HttpClient, ProxyMode};
use forgedm_core::probe::probe;
use forgedm_fixture::FixtureServer;
use forgedm_protocol::{ResumeSupport, TransferMode};
use tokio_util::sync::CancellationToken;
use url::Url;

async fn run(server: &FixtureServer, path_and_query: &str) -> forgedm_core::probe::Probed {
    let client = HttpClient::new(ProxyMode::None).unwrap();
    let url = Url::parse(&format!("{}{path_and_query}", server.base_url())).unwrap();
    probe(&client, &url, &CancellationToken::new())
        .await
        .unwrap_or_else(|stop| panic!("probe failed: {stop:?}"))
}

#[tokio::test]
async fn probe_head_then_get_0_0_logged() {
    let server = FixtureServer::start_on(0).await.unwrap();

    let probed = run(&server, "/f/1/100000?t=probe_order").await;

    let log = server.log().for_test("probe_order");
    assert_eq!(log.len(), 2, "exactly a HEAD and a ranged GET: {log:?}");
    assert_eq!(log[0].method, "HEAD");
    assert_eq!(log[0].range, None);
    assert_eq!(log[1].method, "GET");
    assert_eq!(log[1].range.as_deref(), Some("bytes=0-0"));
    assert_eq!(probed.result.total, Some(100_000));
    assert_eq!(probed.result.transfer_mode, TransferMode::SingleRanged);
    assert_eq!(probed.result.resume, ResumeSupport::Reliable);
    assert!(
        probed.result.validators.etag.is_some(),
        "validators come from the GET"
    );
}

#[tokio::test]
async fn probe_tolerates_head_405() {
    let server = FixtureServer::start_on(0).await.unwrap();

    let probed = run(&server, "/f/2/100000?head=0&t=probe_405").await;

    let log = server.log().for_test("probe_405");
    assert_eq!(log[0].method, "HEAD", "the HEAD was tried: {log:?}");
    assert_eq!(log[1].range.as_deref(), Some("bytes=0-0"));
    assert_eq!(probed.result.transfer_mode, TransferMode::SingleRanged);
    assert_eq!(probed.result.total, Some(100_000));
}

#[tokio::test]
async fn probe_never_downloads_whole_body() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let started = Instant::now();

    // A server that ignores ranges answers the probe with the whole (slow, huge) body.
    let ignoring = run(&server, "/f/3/500000000?ranges=0&slow=100000&t=probe_body").await;
    let ranged = run(&server, "/f/4/500000000?t=probe_ranged").await;

    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the probe returned without waiting for the body: {:?}",
        started.elapsed()
    );
    assert!(
        server.log().served_bytes() < 1_000_000,
        "only a sliver of any body was read: {}",
        server.log().served_bytes()
    );
    assert_eq!(
        ignoring.result.transfer_mode,
        TransferMode::SingleNonresumable
    );
    assert_eq!(ignoring.result.resume, ResumeSupport::NotResumable);
    assert_eq!(ranged.result.total, Some(500_000_000));
}
