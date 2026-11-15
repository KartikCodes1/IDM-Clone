//! S1.1 URL policy tests (THREAT_MODEL NET-05, NET-06, SEC-01).

use forgedm_core::url_policy::{
    HostClass, MAX_URL_BYTES, SecretUrl, UrlError, classify_host, redacted_display,
    validate_user_url,
};
use forgedm_protocol::InvalidUrlReason;
use proptest::prelude::*;
use url::Url;

/// The rejection reason, or `None` when the input was accepted.
fn reason_of(input: &str) -> Option<InvalidUrlReason> {
    validate_user_url(input).err().map(UrlError::reason)
}

#[test]
fn scheme_allowlist() {
    for ok in [
        "http://example.com/a",
        "https://example.com/a",
        "HTTPS://EXAMPLE.com/a",
    ] {
        assert!(validate_user_url(ok).is_ok(), "{ok} should be accepted");
    }
    for bad in [
        "ftp://example.com/a",
        "file:///c:/windows/win.ini",
        "javascript:alert(1)",
        "data:text/plain,hi",
        "ws://example.com/",
        "mailto:a@example.com",
        "blob:https://example.com/x",
    ] {
        assert_eq!(reason_of(bad), Some(InvalidUrlReason::Scheme), "{bad}");
    }
    for bad in ["not a url", "", "http://", "http://[::1/", "://x"] {
        assert_eq!(reason_of(bad), Some(InvalidUrlReason::Parse), "{bad:?}");
    }
}

#[test]
fn userinfo_rejected() {
    for bad in [
        "http://user@example.com/",
        "http://user:secret@example.com/",
        "https://:secret@example.com/",
        "https://user:@example.com/x?y=1",
    ] {
        assert_eq!(reason_of(bad), Some(InvalidUrlReason::Userinfo), "{bad}");
    }
}

#[test]
fn overlong_and_control_chars_rejected() {
    let prefix = "http://a.example/";
    let exact = format!("{prefix}{}", "a".repeat(MAX_URL_BYTES - prefix.len()));
    assert_eq!(exact.len(), MAX_URL_BYTES);
    assert!(validate_user_url(&exact).is_ok(), "8192 bytes is allowed");
    let over = format!("{exact}a");
    assert_eq!(reason_of(&over), Some(InvalidUrlReason::TooLong));

    // The url crate silently strips tab/CR/LF, so these must be rejected before parsing.
    for bad in [
        "http://a.example/\0",
        "http://a.example/x\ty",
        "http://a.example/x\ny",
        "http://a.example/x\r\ny",
        "http://a.exa\tmple/",
        "http://a.example/\u{7f}",
        "http://a.example/\u{85}",
        "http://a.example/\u{1b}[31m",
    ] {
        assert_eq!(
            reason_of(bad),
            Some(InvalidUrlReason::ControlChars),
            "{bad:?}"
        );
    }
}

#[test]
fn fragment_stripped_on_validation() {
    let secret = validate_user_url("https://example.com/p?q=1#frag-token").unwrap();
    assert_eq!(secret.as_url().fragment(), None);
    assert_eq!(secret.as_url().as_str(), "https://example.com/p?q=1");
    let bare = validate_user_url("https://example.com/p#").unwrap();
    assert_eq!(bare.as_url().fragment(), None);
}

#[test]
fn error_display_never_contains_the_url() {
    let err: UrlError = validate_user_url("http://user:hunter2@example.com/").unwrap_err();
    let text = format!("{err} / {err:?}");
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.contains("example.com"), "{text}");
}

#[test]
fn secret_url_debug_and_display_are_redacted() {
    let secret =
        validate_user_url("https://example.com:8443/dir/file.bin?token=abc123#frag").unwrap();
    let debug = format!("{secret:?}");
    let display = format!("{secret}");
    for text in [&debug, &display] {
        assert!(!text.contains("abc123"), "{text}");
        assert!(!text.contains("token"), "{text}");
        assert!(!text.contains("frag"), "{text}");
    }
    assert!(
        !debug.contains("example.com"),
        "Debug reveals nothing: {debug}"
    );
    assert_eq!(display, "https://example.com:8443/dir/file.bin");
    assert_eq!(secret.redacted().to_string(), display);
    assert_eq!(
        format!("{:?}", secret.redacted()),
        format!("RedactedUrl({display})")
    );
}

/// Compile-time proof that `SecretUrl` and `RedactedUrl` do not implement `Serialize`:
/// the call below is ambiguous (and fails to compile) if either type gains an impl.
#[test]
fn secret_url_has_no_serialize_impl() {
    trait AmbiguousIfSerialize<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSerialize<()> for T {}
    impl<T: ?Sized + serde::Serialize> AmbiguousIfSerialize<u8> for T {}
    <SecretUrl as AmbiguousIfSerialize<_>>::check();
    <forgedm_core::url_policy::RedactedUrl as AmbiguousIfSerialize<_>>::check();
}

/// The class of `host_text` as the url crate parses it, or `None` if it has no host.
fn class_of(host_text: &str) -> Option<HostClass> {
    let url = Url::parse(&format!("http://{host_text}/")).ok()?;
    url.host().map(|host| classify_host(&host))
}

#[test]
fn local_destination_classification() {
    let cases: &[(&str, HostClass)] = &[
        // Loopback, every WHATWG IPv4 spelling.
        ("127.0.0.1", HostClass::Loopback),
        ("127.255.255.254", HostClass::Loopback),
        ("2130706433", HostClass::Loopback),
        ("0x7f.1", HostClass::Loopback),
        ("0x7f000001", HostClass::Loopback),
        ("017700000001", HostClass::Loopback),
        ("127.1", HostClass::Loopback),
        ("[::1]", HostClass::Loopback),
        ("[::ffff:127.0.0.1]", HostClass::Loopback),
        ("[::ffff:7f00:1]", HostClass::Loopback),
        ("[64:ff9b::7f00:1]", HostClass::Loopback),
        ("[::7f00:1]", HostClass::Loopback),
        ("[2002:7f00:1::]", HostClass::Loopback),
        ("[::a00:1]", HostClass::Private),
        ("[2002:a9fe:a9fe::]", HostClass::LinkLocal),
        // Unspecified.
        ("0.0.0.0", HostClass::Unspecified),
        ("0.1.2.3", HostClass::Unspecified),
        ("[::]", HostClass::Unspecified),
        ("[::ffff:0.0.0.0]", HostClass::Unspecified),
        // RFC 1918.
        ("10.0.0.1", HostClass::Private),
        ("10.255.255.255", HostClass::Private),
        ("172.16.0.1", HostClass::Private),
        ("172.31.255.255", HostClass::Private),
        ("192.168.0.1", HostClass::Private),
        ("192.168.255.255", HostClass::Private),
        ("[::ffff:10.1.2.3]", HostClass::Private),
        ("3232235777", HostClass::Private),
        ("[fec0::1]", HostClass::Private),
        // Link-local, including the cloud metadata address.
        ("169.254.0.1", HostClass::LinkLocal),
        ("169.254.169.254", HostClass::LinkLocal),
        ("169.254.255.255", HostClass::LinkLocal),
        ("[fe80::1]", HostClass::LinkLocal),
        ("[febf::1]", HostClass::LinkLocal),
        ("[::ffff:169.254.169.254]", HostClass::LinkLocal),
        // CGNAT 100.64.0.0/10.
        ("100.64.0.1", HostClass::Cgnat),
        ("100.127.255.255", HostClass::Cgnat),
        // Unique local fc00::/7.
        ("[fc00::1]", HostClass::UniqueLocal),
        ("[fd12:3456::1]", HostClass::UniqueLocal),
        // Names.
        ("localhost", HostClass::LocalName),
        ("LOCALHOST", HostClass::LocalName),
        ("localhost.", HostClass::LocalName),
        ("a.localhost", HostClass::LocalName),
        ("a.b.localhost.", HostClass::LocalName),
        // Public.
        ("8.8.8.8", HostClass::Public),
        ("1.1.1.1", HostClass::Public),
        ("172.15.255.255", HostClass::Public),
        ("172.32.0.1", HostClass::Public),
        ("100.63.255.255", HostClass::Public),
        ("100.128.0.1", HostClass::Public),
        ("169.253.255.255", HostClass::Public),
        ("169.255.0.1", HostClass::Public),
        ("11.0.0.1", HostClass::Public),
        ("192.167.255.255", HostClass::Public),
        ("[2606:4700::1111]", HostClass::Public),
        ("[fe7f::1]", HostClass::Public),
        ("[::ffff:8.8.8.8]", HostClass::Public),
        ("[64:ff9b::808:808]", HostClass::Public),
        ("example.com", HostClass::Public),
        ("notlocalhost", HostClass::Public),
        ("localhost.example.com", HostClass::Public),
        ("xlocalhost.com", HostClass::Public),
    ];
    for (text, expected) in cases {
        assert_eq!(class_of(text), Some(*expected), "classification of {text}");
    }
}

#[test]
fn classify_host_accepts_owned_and_borrowed_hosts() {
    let owned: url::Host<String> = url::Host::parse("[::1]").unwrap();
    assert_eq!(classify_host(&owned), HostClass::Loopback);
    let borrowed: url::Host<&str> = url::Host::Domain("localhost");
    assert_eq!(classify_host(&borrowed), HostClass::LocalName);
}

#[test]
fn host_class_is_local_for_everything_but_public() {
    assert!(!HostClass::Public.is_local());
    for class in [
        HostClass::Loopback,
        HostClass::Private,
        HostClass::LinkLocal,
        HostClass::Cgnat,
        HostClass::UniqueLocal,
        HostClass::Unspecified,
        HostClass::LocalName,
    ] {
        assert!(class.is_local(), "{class:?}");
    }
}

proptest! {
    #[test]
    fn redaction_strips_query_fragment_userinfo(
        https in any::<bool>(),
        host in "[a-z]{1,8}\\.[a-z]{2,3}",
        port in proptest::option::of(8000u16..9000),
        segments in proptest::collection::vec("[a-z0-9]{1,6}", 0..4),
        query in proptest::option::of("q[a-zA-Z0-9=&]{1,12}"),
        fragment in proptest::option::of("f[a-z0-9]{1,8}"),
        userinfo in proptest::option::of(("u[a-z]{1,6}", "p[a-z]{1,6}")),
    ) {
        let scheme = if https { "https" } else { "http" };
        let path = format!("/{}", segments.join("/"));
        let port_text = port.map(|p| format!(":{p}")).unwrap_or_default();
        let mut text = format!("{scheme}://");
        if let Some((user, pass)) = &userinfo {
            text.push_str(&format!("{user}:{pass}@"));
        }
        text.push_str(&format!("{host}{port_text}{path}"));
        if let Some(q) = &query {
            text.push_str(&format!("?{q}"));
        }
        if let Some(f) = &fragment {
            text.push_str(&format!("#{f}"));
        }
        let url = Url::parse(&text).unwrap();
        let shown = redacted_display(&url);
        prop_assert_eq!(&shown, &format!("{scheme}://{host}{port_text}{path}"));
        prop_assert!(!shown.contains('?') && !shown.contains('#') && !shown.contains('@'));
    }
}
