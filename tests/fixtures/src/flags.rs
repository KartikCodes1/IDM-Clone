//! Query-string behaviour flags for `/f/{seed}/{size}`.
//!
//! Unknown flags are ignored so later scenarios can be added without breaking older clients.

/// How the `ETag` response header is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EtagMode {
    /// A strong validator (`"seed-size"`).
    #[default]
    Strong,
    /// A weak validator (`W/"seed-size"`).
    Weak,
    /// No `ETag` header.
    None,
    /// A strong validator that changes (together with the content) once `mutate_after`
    /// requests have been served.
    Mutable,
}

/// How a 206 `Content-Range` header is corrupted (`bad_cr=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadContentRange {
    /// The start is one past the served start.
    Start,
    /// The end is one past the served end.
    End,
    /// The total is one more than the real size.
    Total,
    /// Not a byte-range at all.
    Syntax,
    /// The first byte position is greater than the last.
    Inverted,
    /// The first byte position is at or beyond the total.
    GeTotal,
    /// The end and total do not fit in 64 bits.
    Overflow,
}

/// The `Content-Length` lie (`lie_cl=+N|-N`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LieCl {
    /// Claims N more bytes than are sent.
    Over(u64),
    /// Claims N fewer bytes than are sent.
    Under(u64),
}

/// Redirect scenario (`redirect=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectMode {
    /// To another host name (`cross.test`).
    Cross,
    /// To an explicit `http://` URL (a downgrade when the client arrived over TLS).
    Downgrade,
    /// To the request's own URL, forever.
    Loop,
    /// N more hops, then the content.
    Chain(u32),
    /// To a private (RFC 1918) address.
    Private,
    /// To a signed URL that stops working after `ttl_requests` uses.
    SignedExpiring,
}

/// Wire-level violation produced on a raw TCP socket (`raw=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawMode {
    /// Two conflicting `Content-Length` headers.
    DupCl,
    /// Both `Content-Length` and `Transfer-Encoding: chunked`.
    ClAndTe,
    /// A status line with a non-numeric code.
    BadStatus,
    /// A response head far beyond any sane limit.
    HugeHeaders,
    /// A `Location` header with a bare LF followed by an injected header line.
    CrlfLocation,
}

/// Parsed behaviour flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flags {
    /// `head=0` turns HEAD into 405.
    pub head: bool,
    /// `cl=0` omits Content-Length (chunked transfer).
    pub content_length: bool,
    /// `ranges=0` ignores Range and omits Accept-Ranges.
    pub ranges: bool,
    /// `slow=<Bps>` throttles the body to roughly this many bytes per second.
    pub slow_bps: Option<u64>,
    /// `etag=strong|weak|none|mutable`.
    pub etag: EtagMode,
    /// `lm=<http-date>`: sent verbatim as Last-Modified.
    pub last_modified: Option<String>,
    /// `cd=<pct-encoded>`: sent as raw bytes in Content-Disposition.
    pub content_disposition: Option<Vec<u8>>,
    /// `encoding=gzip`: adds a `Content-Encoding: gzip` header only (body stays identity).
    pub gzip_header: bool,
    /// `drop_after=<N>`: abort the connection after N body bytes.
    pub drop_after: Option<u64>,
    /// `t=<test key>`: tags log entries.
    pub test_key: Option<String>,
    /// `bad_cr=...`: corrupt the 206 Content-Range.
    pub bad_cr: Option<BadContentRange>,
    /// `short206=N`: a 206 that covers N fewer bytes than requested (at least one byte stays).
    pub short206: Option<u64>,
    /// `lie_cl=+N|-N` (raw socket path).
    pub lie_cl: Option<LieCl>,
    /// `stall_after=N`: stop sending after N body bytes but keep the connection open.
    pub stall_after: Option<u64>,
    /// `mutate_after=K` (with `etag=mutable`): content and ETag change from request index K.
    pub mutate_after: Option<u64>,
    /// `lm_eq_date=1`: Last-Modified equals Date (a weak validator for If-Range).
    pub lm_eq_date: bool,
    /// `status=<code>`: answer with this error status.
    pub status: Option<u16>,
    /// `retry_after=<seconds|http-date>`: sent verbatim with the `status` response.
    pub retry_after: Option<String>,
    /// `fail_first=K`: only the first K requests get `status` (default: all).
    pub fail_first: Option<u64>,
    /// `max_conns=N`: requests beyond N in flight get 503.
    pub max_conns: Option<u64>,
    /// `multipart=1`: answer ranges as `multipart/byteranges`.
    pub multipart: bool,
    /// `range_200_after=K`: ignore Range from the (K+1)th ranged request on.
    pub range_200_after: Option<u64>,
    /// `slow_from=<offset>`: throttle (see `slow`) only from this absolute content offset.
    pub slow_from: Option<u64>,
    /// `redirect=...`.
    pub redirect: Option<RedirectMode>,
    /// `ttl_requests=K`: uses allowed for a signed redirect target. `0` is accepted and forces
    /// an immediately expired link (403 on first use).
    pub ttl_requests: Option<u64>,
    /// `signed=<token>`: set by the fixture itself on `signed_expiring` redirect targets.
    pub signed: Option<String>,
    /// `auth=basic`: 401 unless an `Authorization: Basic` header is sent.
    pub auth_basic: bool,
    /// `raw=...`: serve through the raw socket path.
    pub raw: Option<RawMode>,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            head: true,
            content_length: true,
            ranges: true,
            slow_bps: None,
            etag: EtagMode::Strong,
            last_modified: None,
            content_disposition: None,
            gzip_header: false,
            drop_after: None,
            test_key: None,
            bad_cr: None,
            short206: None,
            lie_cl: None,
            stall_after: None,
            mutate_after: None,
            lm_eq_date: false,
            status: None,
            retry_after: None,
            fail_first: None,
            max_conns: None,
            multipart: false,
            range_200_after: None,
            slow_from: None,
            redirect: None,
            ttl_requests: None,
            signed: None,
            auth_basic: false,
            raw: None,
        }
    }
}

/// A flag had an unusable value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid value {value:?} for flag `{name}`")]
pub struct FlagError {
    /// Flag name.
    pub name: &'static str,
    /// Offending raw value.
    pub value: String,
}

/// Splits a raw query string into undecoded `(name, value)` pairs.
#[must_use]
pub fn parse_query(query: &str) -> Vec<(&str, &str)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').unwrap_or((p, "")))
        .collect()
}

/// Percent-decodes `s` to raw bytes; `None` on a malformed escape.
#[must_use]
pub fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex_val(*bytes.get(i + 1)?)?;
            let lo = hex_val(*bytes.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    char::from(b)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

fn lookup<'a>(pairs: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn bad(name: &'static str, value: &str) -> FlagError {
    FlagError {
        name,
        value: value.to_owned(),
    }
}

fn decode_text(name: &'static str, raw: &str) -> Result<String, FlagError> {
    let bytes = percent_decode(raw).ok_or_else(|| bad(name, raw))?;
    String::from_utf8(bytes).map_err(|_| bad(name, raw))
}

/// Decodes text that will be sent as a header value, rejecting control characters.
fn decode_header_text(name: &'static str, raw: &str) -> Result<String, FlagError> {
    let text = decode_text(name, raw)?;
    if http::HeaderValue::from_str(&text).is_err() {
        return Err(bad(name, raw));
    }
    Ok(text)
}

fn parse_switch(name: &'static str, raw: Option<&str>) -> Result<bool, FlagError> {
    match raw {
        None | Some("1") => Ok(true),
        Some("0") => Ok(false),
        Some(other) => Err(bad(name, other)),
    }
}

/// A flag that is off unless present: `name=1` turns it on, `name=0` or absence leaves it off.
fn parse_opt_in(name: &'static str, raw: Option<&str>) -> Result<bool, FlagError> {
    raw.map_or(Ok(false), |v| parse_switch(name, Some(v)))
}

fn parse_count(name: &'static str, raw: Option<&str>, min: u64) -> Result<Option<u64>, FlagError> {
    raw.map(|v| {
        v.parse::<u64>()
            .ok()
            .filter(|n| *n >= min)
            .ok_or_else(|| bad(name, v))
    })
    .transpose()
}

const ALLOWED_STATUSES: [u16; 8] = [401, 403, 404, 410, 416, 429, 500, 503];

fn parse_status(raw: Option<&str>) -> Result<Option<u16>, FlagError> {
    raw.map(|v| {
        v.parse::<u16>()
            .ok()
            .filter(|c| ALLOWED_STATUSES.contains(c))
            .ok_or_else(|| bad("status", v))
    })
    .transpose()
}

fn parse_bad_cr(raw: Option<&str>) -> Result<Option<BadContentRange>, FlagError> {
    raw.map(|v| match v {
        "start" => Ok(BadContentRange::Start),
        "end" => Ok(BadContentRange::End),
        "total" => Ok(BadContentRange::Total),
        "syntax" => Ok(BadContentRange::Syntax),
        "inverted" => Ok(BadContentRange::Inverted),
        "ge_total" => Ok(BadContentRange::GeTotal),
        "overflow" => Ok(BadContentRange::Overflow),
        other => Err(bad("bad_cr", other)),
    })
    .transpose()
}

fn parse_lie_cl(raw: Option<&str>) -> Result<Option<LieCl>, FlagError> {
    raw.map(|v| {
        let text = decode_text("lie_cl", v)?;
        let amount = |digits: &str| digits.parse::<u64>().map_err(|_| bad("lie_cl", v));
        match (text.strip_prefix('+'), text.strip_prefix('-')) {
            (Some(n), _) => amount(n).map(LieCl::Over),
            (_, Some(n)) => amount(n).map(LieCl::Under),
            _ => Err(bad("lie_cl", v)),
        }
    })
    .transpose()
}

fn parse_redirect(raw: Option<&str>) -> Result<Option<RedirectMode>, FlagError> {
    raw.map(|v| match v {
        "cross" => Ok(RedirectMode::Cross),
        "downgrade" => Ok(RedirectMode::Downgrade),
        "loop" => Ok(RedirectMode::Loop),
        "private" => Ok(RedirectMode::Private),
        "signed_expiring" => Ok(RedirectMode::SignedExpiring),
        other => other
            .strip_prefix("chain:")
            .and_then(|n| n.parse::<u32>().ok())
            .map(RedirectMode::Chain)
            .ok_or_else(|| bad("redirect", other)),
    })
    .transpose()
}

fn parse_raw(raw: Option<&str>) -> Result<Option<RawMode>, FlagError> {
    raw.map(|v| match v {
        "dup_cl" => Ok(RawMode::DupCl),
        "cl_and_te" => Ok(RawMode::ClAndTe),
        "bad_status" => Ok(RawMode::BadStatus),
        "huge_headers" => Ok(RawMode::HugeHeaders),
        "crlf_location" => Ok(RawMode::CrlfLocation),
        other => Err(bad("raw", other)),
    })
    .transpose()
}

fn parse_signed(raw: Option<&str>) -> Result<Option<String>, FlagError> {
    raw.map(|v| {
        let token_ok = !v.is_empty()
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if token_ok {
            Ok(v.to_owned())
        } else {
            Err(bad("signed", v))
        }
    })
    .transpose()
}

fn parse_etag(raw: Option<&str>) -> Result<EtagMode, FlagError> {
    match raw {
        None | Some("strong") => Ok(EtagMode::Strong),
        Some("weak") => Ok(EtagMode::Weak),
        Some("none") => Ok(EtagMode::None),
        Some("mutable") => Ok(EtagMode::Mutable),
        Some(other) => Err(bad("etag", other)),
    }
}

impl Flags {
    /// Extracts the test key without validating the other flags (used for logging).
    #[must_use]
    pub fn test_key_of(pairs: &[(&str, &str)]) -> Option<String> {
        lookup(pairs, "t").and_then(|v| decode_text("t", v).ok())
    }

    /// Parses all flags from decoded query pairs.
    ///
    /// # Errors
    /// [`FlagError`] naming the first flag whose value is invalid, or whose companion flag
    /// is missing (`slow_from` needs `slow`, `retry_after`/`fail_first` need `status`,
    /// `mutate_after` needs `etag=mutable`).
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Result<Self, FlagError> {
        let flags = Self::parse_all(pairs)?;
        flags.check_companions(pairs)?;
        Ok(flags)
    }

    fn parse_all(pairs: &[(&str, &str)]) -> Result<Self, FlagError> {
        let get = |name: &str| lookup(pairs, name);
        let gzip_header = match get("encoding") {
            None => false,
            Some("gzip") => true,
            Some(other) => return Err(bad("encoding", other)),
        };
        let auth_basic = match get("auth") {
            None => false,
            Some("basic") => true,
            Some(other) => return Err(bad("auth", other)),
        };
        let header_text =
            |name: &'static str| get(name).map(|v| decode_header_text(name, v)).transpose();
        Ok(Self {
            head: parse_switch("head", get("head"))?,
            content_length: parse_switch("cl", get("cl"))?,
            ranges: parse_switch("ranges", get("ranges"))?,
            slow_bps: parse_count("slow", get("slow"), 1)?,
            etag: parse_etag(get("etag"))?,
            last_modified: header_text("lm")?,
            content_disposition: get("cd")
                .map(|v| percent_decode(v).ok_or_else(|| bad("cd", v)))
                .transpose()?,
            gzip_header,
            drop_after: parse_count("drop_after", get("drop_after"), 0)?,
            test_key: Self::test_key_of(pairs),
            bad_cr: parse_bad_cr(get("bad_cr"))?,
            short206: parse_count("short206", get("short206"), 0)?,
            lie_cl: parse_lie_cl(get("lie_cl"))?,
            stall_after: parse_count("stall_after", get("stall_after"), 0)?,
            mutate_after: parse_count("mutate_after", get("mutate_after"), 0)?,
            lm_eq_date: parse_opt_in("lm_eq_date", get("lm_eq_date"))?,
            status: parse_status(get("status"))?,
            retry_after: header_text("retry_after")?,
            fail_first: parse_count("fail_first", get("fail_first"), 0)?,
            max_conns: parse_count("max_conns", get("max_conns"), 1)?,
            multipart: parse_opt_in("multipart", get("multipart"))?,
            range_200_after: parse_count("range_200_after", get("range_200_after"), 0)?,
            slow_from: parse_count("slow_from", get("slow_from"), 0)?,
            redirect: parse_redirect(get("redirect"))?,
            ttl_requests: parse_count("ttl_requests", get("ttl_requests"), 0)?,
            signed: parse_signed(get("signed"))?,
            auth_basic,
            raw: parse_raw(get("raw"))?,
        })
    }

    fn check_companions(&self, pairs: &[(&str, &str)]) -> Result<(), FlagError> {
        let raw_of = |name: &str| lookup(pairs, name).unwrap_or("");
        if self.slow_from.is_some() && self.slow_bps.is_none() {
            return Err(bad("slow_from", raw_of("slow_from")));
        }
        if self.status.is_none() {
            if self.retry_after.is_some() {
                return Err(bad("retry_after", raw_of("retry_after")));
            }
            if self.fail_first.is_some() {
                return Err(bad("fail_first", raw_of("fail_first")));
            }
        }
        if self.mutate_after.is_some() && self.etag != EtagMode::Mutable {
            return Err(bad("mutate_after", raw_of("mutate_after")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BadContentRange, EtagMode, Flags, LieCl, RawMode, RedirectMode, parse_query, percent_decode,
    };

    fn parse(q: &str) -> Result<Flags, super::FlagError> {
        Flags::from_pairs(&parse_query(q))
    }

    #[test]
    fn percent_decode_cases() {
        assert_eq!(percent_decode("a%20b"), Some(b"a b".to_vec()));
        assert_eq!(percent_decode("%ff%00"), Some(vec![0xff, 0]));
        assert_eq!(percent_decode(""), Some(Vec::new()));
        assert_eq!(percent_decode("%"), None);
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%zz"), None);
    }

    #[test]
    fn parse_query_splits_pairs() {
        assert_eq!(
            parse_query("a=1&&b&c="),
            vec![("a", "1"), ("b", ""), ("c", "")]
        );
        assert!(parse_query("").is_empty());
    }

    #[test]
    fn defaults_when_empty() {
        assert_eq!(parse("").unwrap(), Flags::default());
    }

    #[test]
    fn valid_values_parse() {
        let f = parse("cl=0&etag=weak&slow=100&drop_after=0&encoding=gzip&t=k%201").unwrap();
        assert!(!f.content_length);
        assert_eq!(f.etag, EtagMode::Weak);
        assert_eq!(f.slow_bps, Some(100));
        assert_eq!(f.drop_after, Some(0));
        assert!(f.gzip_header);
        assert_eq!(f.test_key.as_deref(), Some("k 1"));
    }

    #[test]
    fn invalid_values_are_rejected() {
        for q in [
            "head=2",
            "etag=bogus",
            "encoding=br",
            "slow=0",
            "slow=x",
            "drop_after=-1",
            "drop_after=99999999999999999999999",
            "lm=%zz",
            "lm=%0d%0a",
            "lm=%ff",
            "cd=%",
        ] {
            assert!(parse(q).is_err(), "{q} should be rejected");
        }
    }

    #[test]
    fn adversarial_flags_parse() {
        let f = parse(
            "bad_cr=ge_total&short206=3&lie_cl=%2B5&stall_after=9&etag=mutable&mutate_after=2\
             &lm_eq_date=1&status=429&retry_after=7&fail_first=2&max_conns=3&multipart=1\
             &range_200_after=4&slow=10&slow_from=8&redirect=chain:3&ttl_requests=2\
             &signed=ab-1_Z&auth=basic&raw=cl_and_te",
        )
        .unwrap();
        assert_eq!(f.bad_cr, Some(BadContentRange::GeTotal));
        assert_eq!(f.short206, Some(3));
        assert_eq!(f.lie_cl, Some(LieCl::Over(5)));
        assert_eq!(f.stall_after, Some(9));
        assert_eq!(f.etag, EtagMode::Mutable);
        assert_eq!(f.mutate_after, Some(2));
        assert!(f.lm_eq_date && f.multipart && f.auth_basic);
        assert_eq!(f.status, Some(429));
        assert_eq!(f.retry_after.as_deref(), Some("7"));
        assert_eq!(f.fail_first, Some(2));
        assert_eq!(f.max_conns, Some(3));
        assert_eq!(f.range_200_after, Some(4));
        assert_eq!(f.slow_from, Some(8));
        assert_eq!(f.redirect, Some(RedirectMode::Chain(3)));
        assert_eq!(f.ttl_requests, Some(2));
        assert_eq!(f.signed.as_deref(), Some("ab-1_Z"));
        assert_eq!(f.raw, Some(RawMode::ClAndTe));
        assert_eq!(parse("lie_cl=-7").unwrap().lie_cl, Some(LieCl::Under(7)));
    }

    #[test]
    fn invalid_adversarial_values_are_rejected() {
        for q in [
            "bad_cr=x",
            "lie_cl=5",
            "lie_cl=%2Bx",
            "status=200",
            "status=abc",
            "max_conns=0",
            "redirect=chain:",
            "redirect=chain:-1",
            "redirect=sideways",
            "raw=none",
            "auth=digest",
            "signed=a%20b",
            "signed=",
            "slow_from=5",
            "retry_after=5",
            "fail_first=1",
            "mutate_after=1",
            "multipart=2",
            "retry_after=1&status=503&fail_first=-1",
        ] {
            assert!(parse(q).is_err(), "{q} should be rejected");
        }
    }
}
