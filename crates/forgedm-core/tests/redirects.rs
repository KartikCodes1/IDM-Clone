//! Redirect handling through the real engine (PLAN S1.12, ARCHITECTURE 4.9): manual following,
//! limits, downgrade and local-destination refusal, and what is (not) sent on each hop.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;
#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use forgedm_core::client::{ClientOptions, ProxyMode};
use forgedm_core::http::redirect::{HopDecision, RedirectError, RedirectState, next_hop};
use forgedm_fixture::FixtureServer;
use forgedm_protocol::{DownloadErrorCode, DownloadState};
use support::{Content, Harness, Reply, Req, Scripted};
use url::Url;

#[tokio::test(flavor = "multi_thread")]
async fn redirect_chain_final_url_recorded_sealed() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();
    let original = format!("{}/f/5/200000?redirect=chain:3", server.base_url());

    let (id, event) = h.run(&original, "chain.bin", 4).await;

    Harness::completed(&event);
    let recorded = h.store.row(id).unwrap().final_url.expect("a final URL");
    let exposed = recorded.as_url().as_str().to_owned();
    assert!(exposed.contains("redirect=chain:0"), "{exposed}");
    assert_ne!(exposed, original, "the redirect target, not the original");
    let shown = format!("{recorded:?} {recorded} {}", recorded.redacted().as_str());
    assert!(
        !shown.contains("redirect="),
        "never shown with its query: {shown}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn loop_and_over_limit_rejected() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();

    let (_, looped) = h
        .run(
            &format!("{}/f/5/1000?redirect=loop", server.base_url()),
            "a.bin",
            4,
        )
        .await;
    let (_, long) = h
        .run(
            &format!("{}/f/5/1000?redirect=chain:11", server.base_url()),
            "b.bin",
            4,
        )
        .await;

    assert_eq!(
        Harness::failure_code(&looped),
        DownloadErrorCode::RedirectLoop
    );
    assert_eq!(
        Harness::failure_code(&long),
        DownloadErrorCode::TooManyRedirects
    );
    assert!(
        h.sleeper.sleeps().is_empty(),
        "redirect failures are never retried"
    );
}

/// A trusted local TLS server is impossible here (the build bans custom trust roots, see
/// `clippy.toml`), so the https to http refusal is checked at the engine's own decision
/// point: the hop function every request goes through, and the code it maps to.
#[test]
fn https_to_http_rejected() {
    let secure = Url::parse("https://files.example/a.bin").unwrap();
    let state = RedirectState::new(&secure);

    let decision = next_hop(&state, 302, Some("http://files.example/a.bin"));

    let HopDecision::Reject(error) = decision else {
        panic!("a downgrade must be refused, got {decision:?}");
    };
    assert_eq!(error, RedirectError::Downgrade);
    assert_eq!(error.code(), DownloadErrorCode::RedirectDowngrade);
}

#[tokio::test(flavor = "multi_thread")]
async fn public_to_loopback_literal_rejected() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let mut options = ClientOptions::new(ProxyMode::None);
    options.resolve = vec![("a.test".to_owned(), server.addr())];
    let h = Harness::with_client(options);
    // `a.test` is a public name that resolves to the fixture; the server redirects to the
    // literal 127.0.0.1.
    let url = format!(
        "http://a.test:{}/f/6/1000?redirect=downgrade",
        server.addr().port()
    );

    let (id, event) = h.run(&url, "p.bin", 4).await;

    assert_eq!(
        Harness::failure_code(&event),
        DownloadErrorCode::RedirectBlocked
    );
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_referer_sent() {
    let target = Scripted::start(|_: &Req| Reply::Content(Content::new(7, 5000))).await;
    let location = format!("http://{}/final.bin", target.addr);
    let hopper =
        Scripted::start(move |_: &Req| Reply::Status(302, vec![("Location", location.clone())]))
            .await;
    let h = Harness::new();

    let (_, event) = h.run(&hopper.url("/start.bin"), "r.bin", 4).await;

    Harness::completed(&event);
    let seen: Vec<Req> = hopper
        .requests()
        .into_iter()
        .chain(target.requests())
        .collect();
    assert!(seen.len() >= 4, "both hops were requested");
    for req in &seen {
        assert!(req.header("referer").is_none(), "{req:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_auth_or_cookie_seen_cross_host() {
    let target = Scripted::start(|_: &Req| Reply::Content(Content::new(8, 5000))).await;
    let location = format!("http://cross.test:{}/final.bin", target.addr.port());
    let origin =
        Scripted::start(move |_: &Req| Reply::Status(302, vec![("Location", location.clone())]))
            .await;
    let mut options = ClientOptions::new(ProxyMode::None);
    options.resolve = vec![("cross.test".to_owned(), target.addr)];
    let h = Harness::with_client(options);

    let (_, event) = h.run(&origin.url("/start.bin"), "c.bin", 4).await;

    Harness::completed(&event);
    let cross: Vec<Req> = target.requests();
    assert!(!cross.is_empty());
    assert!(
        cross.iter().all(|r| r
            .header("host")
            .is_some_and(|v| v.starts_with("cross.test"))),
        "the requests really went to the other host"
    );
    for req in origin.requests().iter().chain(cross.iter()) {
        for name in ["authorization", "cookie", "proxy-authorization", "referer"] {
            assert!(req.header(name).is_none(), "{name} on {req:?}");
        }
    }
}
