//! S1.2 redirect policy tests (THREAT_MODEL NET-04, NET-06, Inv-09, Inv-10; ARCHITECTURE 4.9).

use forgedm_core::http::redirect::{
    BlockReason, HopDecision, MAX_LOCATION_BYTES, MAX_REDIRECTS, RedirectError, RedirectState,
    is_redirect_status, next_hop,
};
use url::Url;

fn url(s: &str) -> Url {
    Url::parse(s).unwrap_or_else(|e| unreachable!("bad test URL {s}: {e}"))
}

fn follow(state: &RedirectState, status: u16, location: &str) -> forgedm_core::http::redirect::Hop {
    match next_hop(state, status, Some(location)) {
        HopDecision::Follow(hop) => *hop,
        other => unreachable!("expected Follow for {location}, got {other:?}"),
    }
}

fn rejection(state: &RedirectState, status: u16, location: &str) -> RedirectError {
    match next_hop(state, status, Some(location)) {
        HopDecision::Reject(e) => e,
        other => unreachable!("expected Reject for {location}, got {other:?}"),
    }
}

#[test]
fn redirect_statuses() {
    for s in [301, 302, 303, 307, 308] {
        assert!(is_redirect_status(s), "{s}");
    }
    for s in [200, 206, 300, 304, 305, 306, 309, 399, 404] {
        assert!(!is_redirect_status(s), "{s}");
    }
    let state = RedirectState::new(&url("http://example.com/"));
    assert_eq!(next_hop(&state, 200, Some("/x")), HopDecision::Final);
    assert_eq!(next_hop(&state, 300, Some("/x")), HopDecision::Final);
}

#[test]
fn redirect_hop_limit_10() {
    assert_eq!(MAX_REDIRECTS, 10);
    let mut state = RedirectState::new(&url("http://example.com/start"));
    for i in 1..=10 {
        let hop = follow(&state, 302, &format!("/hop{i}"));
        assert_eq!(hop.url.path(), format!("/hop{i}"));
        state = hop.state;
        assert_eq!(state.hops(), i);
    }
    assert_eq!(
        rejection(&state, 302, "/hop11"),
        RedirectError::TooManyRedirects
    );
    // The original state is untouched by a decision (no mutation).
    assert_eq!(state.hops(), 10);
}

#[test]
fn redirect_loop_detected() {
    let start = RedirectState::new(&url("http://example.com/a"));
    // Self redirect.
    assert_eq!(rejection(&start, 301, "/a"), RedirectError::Loop);
    // A -> B -> A.
    let b = follow(&start, 302, "/b");
    assert_eq!(rejection(&b.state, 302, "/a"), RedirectError::Loop);
    // The fragment does not make a URL distinct; scheme/host case does not either.
    assert_eq!(
        rejection(&b.state, 302, "http://EXAMPLE.com/a#frag"),
        RedirectError::Loop
    );
    // The query does.
    let q = follow(&b.state, 302, "/a?x=1");
    assert_eq!(q.url.query(), Some("x=1"));
    // Default port spelled out is the same URL.
    assert_eq!(
        rejection(&b.state, 302, "http://example.com:80/a"),
        RedirectError::Loop
    );
}

#[test]
fn redirect_downgrade_rejected() {
    let state = RedirectState::new(&url("https://example.com/a"));
    assert_eq!(
        rejection(&state, 302, "http://example.com/b"),
        RedirectError::Downgrade
    );
    assert_eq!(
        rejection(&state, 307, "http://other.example/b"),
        RedirectError::Downgrade
    );
    // After an https hop, still https: an http hop later is also a downgrade.
    let hop = follow(&state, 302, "https://cdn.example/b");
    assert_eq!(
        rejection(&hop.state, 302, "http://cdn.example/c"),
        RedirectError::Downgrade
    );
    // Upgrade and same-scheme are fine.
    let plain = RedirectState::new(&url("http://example.com/a"));
    assert_eq!(
        follow(&plain, 301, "https://example.com/a").url.scheme(),
        "https"
    );
    assert_eq!(
        follow(&plain, 301, "http://example.com/other").url.scheme(),
        "http"
    );
    // Downgrade is judged against the current hop, so https -> https -> http is caught even
    // when the first URL was http that upgraded.
    let up = follow(&plain, 301, "https://example.com/a");
    assert_eq!(
        rejection(&up.state, 301, "http://example.com/a"),
        RedirectError::Downgrade
    );
}

#[test]
fn redirect_non_http_scheme_rejected() {
    let state = RedirectState::new(&url("http://example.com/a"));
    for loc in [
        "ftp://example.com/x",
        "file:///c:/windows/win.ini",
        "javascript:alert(1)",
        "data:text/plain,hi",
        "ws://example.com/",
        "mailto:a@example.com",
        "about:blank",
    ] {
        assert_eq!(
            rejection(&state, 302, loc),
            RedirectError::Blocked(BlockReason::Scheme),
            "{loc}"
        );
    }
}

#[test]
fn redirect_public_to_local_rejected_unless_origin_local() {
    let public = RedirectState::new(&url("http://example.com/a"));
    for loc in [
        "http://127.0.0.1/x",
        "http://2130706433/x",
        "http://0x7f.1/x",
        "http://127.1/x",
        "http://[::1]/x",
        "http://[::ffff:127.0.0.1]/x",
        "http://0.0.0.0/x",
        "http://[::]/x",
        "http://10.1.2.3/x",
        "http://172.16.0.1/x",
        "http://192.168.1.1/x",
        "http://169.254.169.254/latest/meta-data",
        "http://100.64.0.1/x",
        "http://[fc00::1]/x",
        "http://[fe80::1]/x",
        "http://localhost/x",
        "http://a.localhost/x",
        "http://LOCALHOST:8080/x",
    ] {
        assert_eq!(
            rejection(&public, 302, loc),
            RedirectError::Blocked(BlockReason::PrivateDestination),
            "{loc}"
        );
    }
    // Relative and scheme-relative locations are classified after resolution.
    assert_eq!(
        rejection(&public, 302, "//127.0.0.1/x"),
        RedirectError::Blocked(BlockReason::PrivateDestination)
    );
    // The block applies at every hop, not only the first.
    let mid = follow(&public, 302, "http://cdn.example/x");
    assert_eq!(
        rejection(&mid.state, 302, "http://192.168.0.1/x"),
        RedirectError::Blocked(BlockReason::PrivateDestination)
    );
    // Public targets are fine.
    assert_eq!(
        follow(&public, 302, "http://93.184.216.34/x")
            .url
            .host_str(),
        Some("93.184.216.34")
    );

    // When the user themselves typed a local URL, local hops are allowed.
    for origin in [
        "http://127.0.0.1:8080/a",
        "http://localhost/a",
        "http://[::1]:9/a",
        "http://192.168.1.5/a",
    ] {
        let local = RedirectState::new(&url(origin));
        assert!(local.original_is_local(), "{origin}");
        assert_eq!(
            follow(&local, 302, "http://127.0.0.1:9090/x").url.port(),
            Some(9090)
        );
        assert_eq!(
            follow(&local, 302, "http://10.0.0.1/x").url.host_str(),
            Some("10.0.0.1")
        );
    }
    assert!(!public.original_is_local());
}

#[test]
fn redirect_location_resolved_relative_and_capped_8kib() {
    assert_eq!(MAX_LOCATION_BYTES, 8 * 1024);
    let state = RedirectState::new(&url("http://a.example:8080/p/q/r?old=1#frag"));
    assert_eq!(
        follow(&state, 302, "../x?y=1").url.as_str(),
        "http://a.example:8080/p/x?y=1"
    );
    assert_eq!(
        follow(&state, 302, "/abs").url.as_str(),
        "http://a.example:8080/abs"
    );
    assert_eq!(
        follow(&state, 302, "s").url.as_str(),
        "http://a.example:8080/p/q/s"
    );
    assert_eq!(
        follow(&state, 302, "//b.example/z").url.as_str(),
        "http://b.example/z"
    );
    assert_eq!(
        follow(&state, 302, "?only=query").url.as_str(),
        "http://a.example:8080/p/q/r?only=query"
    );
    // The resulting URL never carries a fragment.
    assert_eq!(follow(&state, 302, "/x#section").url.fragment(), None);

    // Exactly 8 KiB is allowed; one byte more is not.
    let ok = format!("/{}", "a".repeat(MAX_LOCATION_BYTES - 1));
    assert_eq!(ok.len(), MAX_LOCATION_BYTES);
    assert!(matches!(
        next_hop(&state, 302, Some(&ok)),
        HopDecision::Follow(_)
    ));
    let too_long = format!("/{}", "a".repeat(MAX_LOCATION_BYTES));
    assert_eq!(
        rejection(&state, 302, &too_long),
        RedirectError::Blocked(BlockReason::LocationTooLong)
    );
}

#[test]
fn redirect_bad_locations_rejected() {
    let state = RedirectState::new(&url("http://example.com/a"));
    assert_eq!(
        next_hop(&state, 302, None),
        HopDecision::Reject(RedirectError::Blocked(BlockReason::MissingLocation))
    );
    assert_eq!(
        rejection(&state, 302, "   "),
        RedirectError::Blocked(BlockReason::MissingLocation)
    );
    assert_eq!(
        rejection(&state, 302, "http://user:pw@example.org/x"),
        RedirectError::Blocked(BlockReason::Userinfo)
    );
    assert_eq!(
        rejection(&state, 302, "/x\r\nSet-Cookie: a=b"),
        RedirectError::Blocked(BlockReason::ControlChars)
    );
    assert_eq!(
        rejection(&state, 302, "http://exa\tmple.org/x"),
        RedirectError::Blocked(BlockReason::ControlChars)
    );
    assert_eq!(
        rejection(&state, 302, "http://[::1/x"),
        RedirectError::Blocked(BlockReason::InvalidLocation)
    );
    assert_eq!(
        rejection(&state, 302, "http://"),
        RedirectError::Blocked(BlockReason::InvalidLocation)
    );
}

#[test]
fn redirect_errors_map_to_download_error_codes() {
    use forgedm_protocol::DownloadErrorCode as C;
    assert_eq!(RedirectError::TooManyRedirects.code(), C::TooManyRedirects);
    assert_eq!(RedirectError::Loop.code(), C::RedirectLoop);
    assert_eq!(RedirectError::Downgrade.code(), C::RedirectDowngrade);
    assert_eq!(
        RedirectError::Blocked(BlockReason::Scheme).code(),
        C::RedirectBlocked
    );
}

#[test]
fn origin_bound_headers_only_to_original_origin() {
    let start = RedirectState::new(&url("https://origin.example/a"));
    assert!(start.allows_origin_bound_headers(&url("https://origin.example/other")));
    assert!(start.allows_origin_bound_headers(&url("https://ORIGIN.example:443/other")));
    // Different host, port or scheme: not the origin.
    assert!(!start.allows_origin_bound_headers(&url("https://other.example/a")));
    assert!(!start.allows_origin_bound_headers(&url("https://origin.example:8443/a")));
    assert!(!start.allows_origin_bound_headers(&url("http://origin.example/a")));
    assert!(!start.allows_origin_bound_headers(&url("https://sub.origin.example/a")));

    // Same-origin hop keeps the headers.
    let same = follow(&start, 302, "/b");
    assert!(same.send_origin_bound_headers);

    // Cross-origin hop drops them.
    let cross = follow(&start, 302, "https://cdn.example/b");
    assert!(!cross.send_origin_bound_headers);

    // The decision is against the ORIGINAL origin, never the previous hop: a second hop on
    // the same foreign host (previous-hop comparison would say "same origin") still drops.
    let cross2 = follow(&cross.state, 302, "https://cdn.example/c");
    assert!(!cross2.send_origin_bound_headers);

    // Returning to the original origin from a foreign one is allowed (it is the origin).
    let back = follow(&cross2.state, 302, "https://origin.example/d");
    assert!(back.send_origin_bound_headers);

    // Same host, different port is a different origin.
    let port = follow(&start, 302, "https://origin.example:8443/e");
    assert!(!port.send_origin_bound_headers);
}
