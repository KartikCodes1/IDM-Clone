//! URL policy: scheme, syntax, redaction and destination classification.
//!
//! Pure code (no I/O, no DNS). Covers THREAT_MODEL NET-05 (unsafe URL syntax), NET-06
//! (private destinations) and SEC-01 (URLs never appear unredacted in logs or errors).

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use forgedm_protocol::InvalidUrlReason;
use url::{Host, Url};

/// Longest accepted URL in bytes, before and after normalisation (Appendix A.9).
pub const MAX_URL_BYTES: usize = 8192;

/// Why a user-supplied URL was rejected. Variants carry no URL text, so the error can be
/// logged and returned to clients as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    /// The text is not a parseable URL.
    #[error("the URL could not be parsed")]
    Parse,
    /// The scheme is not http or https.
    #[error("only http and https URLs are supported")]
    Scheme,
    /// The URL is longer than 8 KiB.
    #[error("the URL is longer than 8192 bytes")]
    TooLong,
    /// The URL embeds a user name or password.
    #[error("URLs with embedded credentials are not accepted")]
    Userinfo,
    /// The URL has no usable host.
    #[error("the URL has no host")]
    Host,
    /// The URL contains a control character.
    #[error("the URL contains a control character")]
    ControlChars,
}

impl UrlError {
    /// The wire reason for an `invalid_url` error (Appendix A.7).
    #[must_use]
    pub const fn reason(self) -> InvalidUrlReason {
        match self {
            Self::Parse => InvalidUrlReason::Parse,
            Self::Scheme => InvalidUrlReason::Scheme,
            Self::TooLong => InvalidUrlReason::TooLong,
            Self::Userinfo => InvalidUrlReason::Userinfo,
            Self::Host => InvalidUrlReason::Host,
            Self::ControlChars => InvalidUrlReason::ControlChars,
        }
    }
}

/// A validated download URL. It may carry secrets (query tokens), so `Debug` and
/// `Display` are redacted and there is deliberately no `Serialize` impl. Reading the
/// real URL takes an explicit [`SecretUrl::as_url`] call.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretUrl(Url);

impl SecretUrl {
    /// The full URL. Only the network layer and the sealed store should call this.
    #[must_use]
    pub fn as_url(&self) -> &Url {
        &self.0
    }

    /// The redacted form (`scheme://host[:port]/path`) that is safe to show and log.
    #[must_use]
    pub fn redacted(&self) -> RedactedUrl {
        RedactedUrl(redacted_display(&self.0))
    }
}

impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretUrl(<redacted>)")
    }
}

impl fmt::Display for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redacted_display(&self.0))
    }
}

/// A URL reduced to `scheme://host[:port]/path`: no userinfo, query or fragment. Path
/// segments can still carry tokens (a documented residual, THREAT_MODEL SEC-01).
#[derive(Clone, PartialEq, Eq)]
pub struct RedactedUrl(String);

impl RedactedUrl {
    /// The redacted text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RedactedUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RedactedUrl({})", self.0)
    }
}

impl fmt::Display for RedactedUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validates a URL typed or pasted by the user.
///
/// Accepts only http and https URLs with a host, at most 8 KiB, no control characters
/// and no embedded credentials. The fragment is dropped.
///
/// # Errors
///
/// Returns the [`UrlError`] naming the first rule the input breaks.
pub fn validate_user_url(input: &str) -> Result<SecretUrl, UrlError> {
    if input.len() > MAX_URL_BYTES {
        return Err(UrlError::TooLong);
    }
    // The url crate silently drops tab, CR and LF, so control characters are rejected
    // before parsing rather than after.
    if input.chars().any(char::is_control) {
        return Err(UrlError::ControlChars);
    }
    let mut url = Url::parse(input).map_err(|_| UrlError::Parse)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlError::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::Userinfo);
    }
    if url.host().is_none() {
        return Err(UrlError::Host);
    }
    url.set_fragment(None);
    if url.as_str().len() > MAX_URL_BYTES {
        return Err(UrlError::TooLong);
    }
    Ok(SecretUrl(url))
}

/// Renders `scheme://host[:port]/path`, dropping userinfo, query and fragment.
#[must_use]
pub fn redacted_display(url: &Url) -> String {
    let mut out = String::with_capacity(url.as_str().len());
    out.push_str(url.scheme());
    out.push_str("://");
    out.push_str(url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        out.push(':');
        out.push_str(&port.to_string());
    }
    out.push_str(url.path());
    out
}

/// Where a destination host sits, as far as a literal address or name tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostClass {
    /// Not recognised as local. Reserved ranges outside this list (multicast, documentation
    /// prefixes) land here too; the policy only guards the ranges below.
    Public,
    /// 127.0.0.0/8 or `::1`.
    Loopback,
    /// RFC 1918 (10/8, 172.16/12, 192.168/16) and deprecated site-local `fec0::/10`.
    Private,
    /// 169.254.0.0/16 (including cloud metadata 169.254.169.254) or `fe80::/10`.
    LinkLocal,
    /// Carrier-grade NAT 100.64.0.0/10.
    Cgnat,
    /// IPv6 unique local `fc00::/7`.
    UniqueLocal,
    /// 0.0.0.0/8 or `::`.
    Unspecified,
    /// `localhost` or a name under `.localhost`.
    LocalName,
}

impl HostClass {
    /// Whether the destination is anything other than [`HostClass::Public`].
    #[must_use]
    pub const fn is_local(self) -> bool {
        !matches!(self, Self::Public)
    }
}

/// Classifies a parsed host. IP literals are classified by value, so every spelling the
/// WHATWG parser normalises (`2130706433`, `0x7f.1`, `127.1`, IPv4-mapped IPv6) gives
/// the same answer as its dotted form. DNS names are not resolved.
#[must_use]
pub fn classify_host<S: AsRef<str>>(host: &Host<S>) -> HostClass {
    match host {
        Host::Ipv4(ip) => classify_ipv4(*ip),
        Host::Ipv6(ip) => classify_ipv6(*ip),
        Host::Domain(name) => classify_domain(name.as_ref()),
    }
}

fn classify_ipv4(ip: Ipv4Addr) -> HostClass {
    let [a, b, ..] = ip.octets();
    match (a, b) {
        (0, _) => HostClass::Unspecified,
        (127, _) => HostClass::Loopback,
        (10, _) | (192, 168) => HostClass::Private,
        (172, 16..=31) => HostClass::Private,
        (169, 254) => HostClass::LinkLocal,
        (100, 64..=127) => HostClass::Cgnat,
        _ => HostClass::Public,
    }
}

fn classify_ipv6(ip: Ipv6Addr) -> HostClass {
    if ip.is_unspecified() {
        return HostClass::Unspecified;
    }
    if ip.is_loopback() {
        return HostClass::Loopback;
    }
    if let Some(v4) = embedded_ipv4(ip) {
        return classify_ipv4(v4);
    }
    let first = ip.segments()[0];
    match first {
        0xfc00..=0xfdff => HostClass::UniqueLocal,
        0xfe80..=0xfebf => HostClass::LinkLocal,
        0xfec0..=0xfeff => HostClass::Private,
        _ => HostClass::Public,
    }
}

/// The IPv4 address inside an IPv4-mapped (`::ffff:a.b.c.d`) or NAT64 (`64:ff9b::/96`)
/// address, which would otherwise let a literal reach a local target.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let s = ip.segments();
    if s[..6] == [0x0064, 0xff9b, 0, 0, 0, 0] {
        let [a, b] = s[6].to_be_bytes();
        let [c, d] = s[7].to_be_bytes();
        return Some(Ipv4Addr::new(a, b, c, d));
    }
    // Deprecated IPv4-compatible `::a.b.c.d` (never `::` or `::1`).
    if s[..6] == [0; 6] && s[6] != 0 {
        let [a, b] = s[6].to_be_bytes();
        let [c, d] = s[7].to_be_bytes();
        return Some(Ipv4Addr::new(a, b, c, d));
    }
    // 6to4 `2002:a.b.c.d::/48`.
    if s[0] == 0x2002 {
        let [a, b] = s[1].to_be_bytes();
        let [c, d] = s[2].to_be_bytes();
        return Some(Ipv4Addr::new(a, b, c, d));
    }
    None
}

fn classify_domain(name: &str) -> HostClass {
    let name = name.strip_suffix('.').unwrap_or(name);
    let is_localhost = name.eq_ignore_ascii_case("localhost")
        || name
            .len()
            .checked_sub(".localhost".len())
            .and_then(|at| name.get(at..))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(".localhost"));
    if is_localhost {
        HostClass::LocalName
    } else {
        HostClass::Public
    }
}
