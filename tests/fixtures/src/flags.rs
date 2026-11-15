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
    /// `etag=strong|weak|none`.
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

fn parse_count(name: &'static str, raw: Option<&str>, min: u64) -> Result<Option<u64>, FlagError> {
    raw.map(|v| {
        v.parse::<u64>()
            .ok()
            .filter(|n| *n >= min)
            .ok_or_else(|| bad(name, v))
    })
    .transpose()
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
    /// [`FlagError`] naming the first flag whose value is invalid.
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Result<Self, FlagError> {
        let etag = match lookup(pairs, "etag") {
            None | Some("strong") => EtagMode::Strong,
            Some("weak") => EtagMode::Weak,
            Some("none") => EtagMode::None,
            Some(other) => return Err(bad("etag", other)),
        };
        let gzip_header = match lookup(pairs, "encoding") {
            None => false,
            Some("gzip") => true,
            Some(other) => return Err(bad("encoding", other)),
        };
        Ok(Self {
            head: parse_switch("head", lookup(pairs, "head"))?,
            content_length: parse_switch("cl", lookup(pairs, "cl"))?,
            ranges: parse_switch("ranges", lookup(pairs, "ranges"))?,
            slow_bps: parse_count("slow", lookup(pairs, "slow"), 1)?,
            etag,
            last_modified: lookup(pairs, "lm")
                .map(|v| decode_header_text("lm", v))
                .transpose()?,
            content_disposition: lookup(pairs, "cd")
                .map(|v| percent_decode(v).ok_or_else(|| bad("cd", v)))
                .transpose()?,
            gzip_header,
            drop_after: parse_count("drop_after", lookup(pairs, "drop_after"), 0)?,
            test_key: Self::test_key_of(pairs),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{EtagMode, Flags, parse_query, percent_decode};

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
}
