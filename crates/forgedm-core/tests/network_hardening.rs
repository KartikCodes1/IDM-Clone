//! Network hardening through the real engine (PLAN S1.12, THREAT_MODEL NET-01..NET-11):
//! typed errors for hostile servers, limited retries for unreachable ones, fatal TLS
//! failures, and error text that never carries the URL.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;
#[path = "../src/testing.rs"]
#[allow(dead_code)] // each test file uses part of the shared fakes
mod testing;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use forgedm_core::ports::EngineEvent;
use forgedm_core::transfer::resilience::LIMITED_RETRIES;
use forgedm_fixture::FixtureServer;
use forgedm_protocol::{DownloadErrorCode, DownloadState};
use support::{Harness, Reply, Req, Scripted, WAIT};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{ServerConfig, crypto::aws_lc_rs};

/// A loopback port nothing listens on.
async fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread")]
async fn error_messages_contain_no_url_query() {
    const SECRET: &str = "SECRETTOKEN123";
    let server = Scripted::start(|_: &Req| Reply::Status(404, vec![])).await;
    let dead = closed_port().await;
    let h = Harness::new();
    let urls = [
        server.url(&format!("/p/a.bin?token={SECRET}&sig=ZZ")),
        format!("http://127.0.0.1:{dead}/p/b.bin?token={SECRET}&sig=ZZ"),
    ];

    let mut ids = Vec::new();
    for (n, url) in urls.iter().enumerate() {
        let (id, event) = h.run(url, &format!("e{n}.bin"), 4).await;
        assert!(matches!(event, EngineEvent::Failed { .. }), "{event:?}");
        ids.push(id);
    }

    let mut everything = format!("{:?}", h.sink.events());
    for id in ids {
        everything.push_str(&format!("{:?}", h.store.row(id).unwrap().events));
    }
    assert!(
        !everything.contains(SECRET),
        "the token leaked: {everything}"
    );
    assert!(
        !everything.contains("sig=ZZ"),
        "the query leaked: {everything}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_protocol_violations_typed_errors_no_panic() {
    let server = FixtureServer::start_on(0).await.unwrap();
    let h = Harness::new();
    let modes = [
        "dup_cl",
        "cl_and_te",
        "bad_status",
        "huge_headers",
        "crlf_location",
    ];

    for mode in modes {
        let url = format!("{}/f/1/1000?raw={mode}", server.base_url());
        let (id, event) = h.run(&url, &format!("{mode}.bin"), 4).await;

        if let EngineEvent::Completed { final_path, .. } = &event {
            // hyper resolves "Content-Length and chunked" in favour of chunked framing, as
            // RFC 9112 requires; what was written must still be exactly the content.
            assert_eq!(
                mode, "cl_and_te",
                "only conflicting framing may be tolerated"
            );
            forgedm_fixture::verify::file_matches(final_path, 1, 1000).unwrap();
            continue;
        }
        let error = Harness::failure(&event);
        assert!(
            matches!(
                error.code,
                DownloadErrorCode::RangeProtocolViolation
                    | DownloadErrorCode::RedirectBlocked
                    | DownloadErrorCode::HttpStatus
            ),
            "{mode}: {error:?}"
        );
        assert_eq!(h.store.state(id), Some(DownloadState::Failed), "{mode}");
        assert!(!error.message.is_empty());
    }
    assert!(
        h.sleeper.sleeps().is_empty(),
        "malformed framing is never retried"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dns_failure_limited_retry() {
    let h = Harness::new();

    let (_, event) = h.run("http://no-such-host.invalid/x.bin", "x.bin", 1).await;

    assert_eq!(Harness::failure_code(&event), DownloadErrorCode::DnsFailure);
    assert_eq!(
        h.sleeper.sleeps().len(),
        usize::try_from(LIMITED_RETRIES).unwrap(),
        "a few retries, then the error is final"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_limited_retry() {
    let dead = closed_port().await;
    let h = Harness::new();

    let (_, event) = h
        .run(&format!("http://127.0.0.1:{dead}/x.bin"), "x.bin", 1)
        .await;

    assert_eq!(
        Harness::failure_code(&event),
        DownloadErrorCode::NetworkConnect
    );
    assert_eq!(
        h.sleeper.sleeps().len(),
        usize::try_from(LIMITED_RETRIES).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_handshake_failure_fatal_not_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut hello = [0u8; 512];
                let _ = stream.read(&mut hello).await;
                // Not a TLS record: the handshake cannot succeed.
                let _ = stream.write_all(b"HTTP/1.1 400 Not TLS\r\n\r\n").await;
                let _ = stream.shutdown().await;
            });
        }
    });
    let h = Harness::new();

    let (_, event) = h
        .run(&format!("https://127.0.0.1:{port}/x.bin"), "x.bin", 1)
        .await;

    assert_eq!(Harness::failure_code(&event), DownloadErrorCode::TlsFailure);
    assert!(
        h.sleeper.sleeps().is_empty(),
        "a TLS failure is never retried"
    );
    // The optional HEAD and the probing GET: nothing after them.
    assert!(accepted.load(Ordering::SeqCst) <= 2);
}

/// A TLS server with a fresh self-signed certificate; returns its port and a handshake
/// counter.
async fn self_signed_server() -> (u16, Arc<AtomicUsize>) {
    let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certified.cert.der().clone()], key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                // The client aborts the handshake: the certificate is untrusted.
                let _ = acceptor.accept(stream).await;
            });
        }
    });
    (port, attempts)
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_untrusted_cert_fatal() {
    let (port, attempts) = self_signed_server().await;
    let h = Harness::new();

    let (id, event) = h
        .run(&format!("https://127.0.0.1:{port}/x.bin"), "x.bin", 1)
        .await;

    assert_eq!(Harness::failure_code(&event), DownloadErrorCode::TlsFailure);
    assert_eq!(h.store.state(id), Some(DownloadState::Failed));
    assert!(
        h.sleeper.sleeps().is_empty(),
        "never retried, never bypassed"
    );
    assert!(attempts.load(Ordering::SeqCst) <= 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn content_disposition_over_4kib_ignored() {
    let size = 4000;
    let big = format!(
        "attachment; filename=\"evil.bin\"; x=\"{}\"",
        "a".repeat(5000)
    );
    let server =
        Scripted::start(move |req: &Req| Reply::Raw(raw_with_disposition(&big, size, req))).await;
    let h = Harness::new();
    let id = h.enqueue_unnamed(&server.url("/dir/real-name.bin")).await;

    let event = h.sink.wait_terminal(id, WAIT).await;

    let (path, _) = Harness::completed(&event);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        name, "real-name.bin",
        "the 5 KiB header must not pick the name"
    );
}

fn raw_with_disposition(disposition: &str, size: u64, req: &Req) -> Vec<u8> {
    let (status, first, last) = match req.range {
        Some((a, b)) => (206, a, b.unwrap_or(size - 1).min(size - 1)),
        None => (200, 0, size - 1),
    };
    let len = last - first + 1;
    let mut out = format!(
        "HTTP/1.1 {status} OK\r\nContent-Length: {len}\r\nETag: \"cd\"\r\nAccept-Ranges: bytes\r\nContent-Disposition: {disposition}\r\nConnection: close\r\n"
    );
    if status == 206 {
        out.push_str(&format!("Content-Range: bytes {first}-{last}/{size}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    if req.method != "HEAD" {
        let mut body = vec![0u8; usize::try_from(len).unwrap()];
        forgedm_fixture::content::fill(3, first, &mut body);
        bytes.extend(body);
    }
    bytes
}
