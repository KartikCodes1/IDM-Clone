//! ETag and Last-Modified validators and resume identity (ADR-021, RFC 9110 section 8.8).
//!
//! Only strong validators identify a representation for resume: a strong ETag, or (when
//! there is no ETag at all) a Last-Modified at least one second older than `Date`.

use std::time::Duration;

use thiserror::Error;

/// Longest ETag header value accepted (including quotes and the `W/` prefix).
pub const MAX_ETAG_BYTES: usize = 1024;

/// Why an ETag value was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EntityTagError {
    /// Not `[W/]"etagc*"`.
    #[error("malformed entity tag")]
    Syntax,
    /// Longer than [`MAX_ETAG_BYTES`].
    #[error("entity tag too long")]
    TooLong,
}

/// A parsed entity tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityTag {
    weak: bool,
    opaque: String,
}

/// `etagc`: `%x21 / %x23-7E / obs-text` (RFC 9110 section 8.8.3).
const fn is_etag_char(c: char) -> bool {
    c == '\u{21}' || matches!(c, '\u{23}'..='\u{7e}') || c >= '\u{80}'
}

impl EntityTag {
    /// Parses `"opaque"` or `W/"opaque"`. The `W/` prefix is case-sensitive.
    ///
    /// # Errors
    /// [`EntityTagError`] for a malformed or oversized value.
    pub fn parse(input: &str) -> Result<Self, EntityTagError> {
        let value = input.trim_matches([' ', '\t']);
        if value.len() > MAX_ETAG_BYTES {
            return Err(EntityTagError::TooLong);
        }
        let (weak, quoted) = match value.strip_prefix("W/") {
            Some(rest) => (true, rest),
            None => (false, value),
        };
        let opaque = quoted
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .filter(|inner| inner.chars().all(is_etag_char))
            .ok_or(EntityTagError::Syntax)?;
        Ok(Self {
            weak,
            opaque: opaque.to_owned(),
        })
    }

    /// Whether this is a weak tag (`W/` prefix).
    #[must_use]
    pub const fn is_weak(&self) -> bool {
        self.weak
    }

    /// The opaque tag without quotes.
    #[must_use]
    pub fn opaque(&self) -> &str {
        &self.opaque
    }

    /// Strong comparison: both tags strong and byte-identical.
    #[must_use]
    pub fn strong_eq(&self, other: &Self) -> bool {
        !self.weak && !other.weak && self.opaque == other.opaque
    }
}

/// Raw validator headers of one response, kept as received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    /// `ETag` header value.
    pub etag: Option<String>,
    /// `Last-Modified` header value.
    pub last_modified: Option<String>,
    /// `Date` header value.
    pub date: Option<String>,
}

/// Whether `Last-Modified` is a strong validator: `Date >= Last-Modified + 1 s`. A missing
/// or unparseable date makes it weak.
#[must_use]
pub fn last_modified_is_strong(last_modified: &str, date: Option<&str>) -> bool {
    let (Ok(modified), Some(Ok(date))) = (
        httpdate::parse_http_date(last_modified.trim()),
        date.map(|d| httpdate::parse_http_date(d.trim())),
    ) else {
        return false;
    };
    modified
        .checked_add(Duration::from_secs(1))
        .is_some_and(|earliest| earliest <= date)
}

/// The strong validator that identifies a representation, as stored for resume.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StrongValidator {
    Etag(EntityTag),
    LastModified(String),
}

fn strong_validator(v: &Validators) -> Option<StrongValidator> {
    match v.etag.as_deref() {
        // A present ETag decides: a weak or malformed one rules out the Last-Modified fallback.
        Some(raw) => EntityTag::parse(raw)
            .ok()
            .filter(|t| !t.is_weak())
            .map(StrongValidator::Etag),
        None => v
            .last_modified
            .as_deref()
            .filter(|lm| last_modified_is_strong(lm, v.date.as_deref()))
            .map(|lm| StrongValidator::LastModified(lm.trim().to_owned())),
    }
}

/// The `If-Range` value to send: a strong ETag, or a strong Last-Modified when there is
/// no ETag at all, byte-identical to what the server sent. Never a weak validator.
#[must_use]
pub fn if_range_value(v: &Validators) -> Option<String> {
    strong_validator(v).map(|s| match s {
        // A stored ETag is always strong, so the wire form is just the quoted opaque tag.
        StrongValidator::Etag(tag) => format!("\"{}\"", tag.opaque()),
        StrongValidator::LastModified(lm) => lm,
    })
}

/// Outcome of comparing a response's validators with the stored identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatorCheck {
    /// Same representation (or nothing stored that could contradict it).
    Same,
    /// The response carries a validator that contradicts the stored one.
    Different,
    /// The response carries no validator of the stored kind: sameness is unproven.
    Unknown,
}

/// What identifies the remote representation across requests and sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeIdentity {
    /// Complete length, `None` while unknown.
    pub total: Option<u64>,
    validator: Option<StrongValidator>,
}

impl ResumeIdentity {
    /// Builds the identity from the probe's total and validators.
    #[must_use]
    pub fn from_validators(total: Option<u64>, v: &Validators) -> Self {
        Self {
            total,
            validator: strong_validator(v),
        }
    }

    /// Whether a strong validator is stored.
    #[must_use]
    pub const fn has_strong_validator(&self) -> bool {
        self.validator.is_some()
    }

    /// Compares a response's validators with the stored identity.
    #[must_use]
    pub fn check(&self, response: &Validators) -> ValidatorCheck {
        match &self.validator {
            None => ValidatorCheck::Same,
            Some(StrongValidator::Etag(stored)) => match response.etag.as_deref() {
                None => ValidatorCheck::Unknown,
                Some(raw) => match EntityTag::parse(raw) {
                    Ok(tag) if stored.strong_eq(&tag) => ValidatorCheck::Same,
                    _ => ValidatorCheck::Different,
                },
            },
            Some(StrongValidator::LastModified(stored)) => {
                match response.last_modified.as_deref() {
                    None => ValidatorCheck::Unknown,
                    Some(lm) if lm.trim() == stored => ValidatorCheck::Same,
                    Some(_) => ValidatorCheck::Different,
                }
            }
        }
    }
}
