//! Range capability decision: validates a response to a ranged request before any byte is
//! written (ARCHITECTURE 4.6, THREAT_MODEL NET-01, NET-02, Inv-01, Inv-07).
//!
//! [`ResponseMeta`] is plain data (no reqwest types) so the whole table is unit-testable.

use thiserror::Error;

use super::content_range::{ContentRange, RangeRequest, parse_decimal};
use super::validators::{ResumeIdentity, ValidatorCheck, Validators};

/// The parts of an HTTP response the range decision needs, as received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseMeta {
    /// HTTP status code.
    pub status: u16,
    /// Raw `Content-Range` value.
    pub content_range: Option<String>,
    /// Raw `Content-Length` value.
    pub content_length: Option<String>,
    /// Raw `Content-Encoding` value (all header lines joined with commas).
    pub content_encoding: Option<String>,
    /// Raw `Content-Type` value.
    pub content_type: Option<String>,
    /// `ETag`, `Last-Modified` and `Date`.
    pub validators: Validators,
}

/// The ranged request that produced a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangedRequest {
    /// The requested window.
    pub range: RangeRequest,
    /// Whether an `If-Range` header was sent. Invariant: callers MUST send `If-Range`
    /// whenever the stored identity has a strong validator. A validator-less response
    /// (notably a 416, which rarely carries validators) is trusted only when this is
    /// true; otherwise `decide` returns `RemoteChanged`, which is deliberate.
    pub if_range_sent: bool,
    /// Segment number the request belongs to (0 is the first segment).
    pub segment_no: u32,
    /// Whether any byte of this download is already durably committed.
    pub has_committed_data: bool,
}

/// A response that breaks the range contract. Maps to `range_protocol_violation{reason}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RangeViolation {
    /// 206 without a `Content-Range`.
    #[error("206 without Content-Range")]
    MissingContentRange,
    /// Unparseable, inverted, overflowing or wrongly-shaped `Content-Range`.
    #[error("invalid Content-Range")]
    BadContentRange,
    /// `Content-Range` does not start at the requested offset.
    #[error("Content-Range starts at the wrong offset")]
    WrongStart,
    /// `Content-Range` extends past the requested window.
    #[error("Content-Range extends past the request")]
    EndBeyondRequest,
    /// `Content-Range` total differs from the stored total.
    #[error("Content-Range total differs from the stored total")]
    TotalMismatch,
    /// `multipart/byteranges` response.
    #[error("multipart byteranges response")]
    Multipart,
    /// `Content-Length` does not match the `Content-Range` span.
    #[error("Content-Length does not match Content-Range")]
    ContentLengthMismatch,
    /// `Content-Length` is not a valid decimal.
    #[error("invalid Content-Length")]
    BadContentLength,
    /// 304 or 412, neither of which the engine can provoke.
    #[error("unexpected status {status}")]
    UnexpectedStatus {
        /// The offending status.
        status: u16,
    },
    /// The server ignored the range after data was already committed.
    #[error("ranges no longer supported")]
    RangesNoLongerSupported,
    /// The 416 does not describe a completed segment.
    #[error("unexpected 416 content range")]
    UnexpectedUnsatisfied,
    /// The caller's request window is empty or inverted (a caller bug).
    #[error("empty or inverted request window")]
    InvalidRequest,
}

impl RangeViolation {
    /// The stable machine reason string for `range_protocol_violation`.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::MissingContentRange => "missing_content_range",
            Self::BadContentRange => "bad_content_range",
            Self::WrongStart => "wrong_start",
            Self::EndBeyondRequest => "end_beyond_request",
            Self::TotalMismatch => "total_mismatch",
            Self::Multipart => "multipart",
            Self::ContentLengthMismatch => "content_length_mismatch",
            Self::BadContentLength => "bad_content_length",
            Self::UnexpectedStatus { .. } => "unexpected_status",
            Self::RangesNoLongerSupported => "ranges_no_longer_supported",
            Self::UnexpectedUnsatisfied => "unexpected_unsatisfied",
            Self::InvalidRequest => "invalid_request",
        }
    }
}

/// What to do with a response to a ranged request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeDecision {
    /// Write only `[start, end_excl)` of the body; `short` means the segment continues.
    Accept {
        /// First byte to write.
        start: u64,
        /// One past the last byte to write.
        end_excl: u64,
        /// The 206 covered less than the request: request the remainder later.
        short: bool,
    },
    /// A full 200 to segment 0 at offset 0: read the body from byte 0 up to `end_excl`.
    Accept200Full {
        /// One past the last byte this segment may take.
        end_excl: u64,
    },
    /// 416 proving the segment already complete.
    SegmentComplete,
    /// The remote file changed: stop all workers and fail.
    RemoteChanged,
    /// The server ignores ranges and nothing is committed: re-plan as single stream.
    RangeUnsupported,
    /// Re-probe the URL before deciding.
    Reprobe,
    /// Non-identity `Content-Encoding`: fail, never retried (Inv-07).
    UnsupportedContentEncoding,
    /// The response breaks the range contract.
    Violation(RangeViolation),
    /// A status this decision does not judge; classify it with the retry rules.
    OtherStatus {
        /// The status.
        status: u16,
    },
}

/// Whether `Content-Encoding` names anything other than `identity`.
pub(crate) fn has_non_identity_encoding(meta: &ResponseMeta) -> bool {
    meta.content_encoding.as_deref().is_some_and(|value| {
        value
            .split(',')
            .map(str::trim)
            .any(|token| !token.is_empty() && !token.eq_ignore_ascii_case("identity"))
    })
}

/// Whether the body is `multipart/byteranges` (or any multipart).
pub(crate) fn is_multipart(meta: &ResponseMeta) -> bool {
    meta.content_type.as_deref().is_some_and(|ct| {
        ct.trim_start()
            .get(..10)
            .is_some_and(|p| p.eq_ignore_ascii_case("multipart/"))
    })
}

/// Parses `Content-Length` strictly (digits only, at most 20 of them).
pub(crate) fn parse_content_length(meta: &ResponseMeta) -> Result<Option<u64>, RangeViolation> {
    meta.content_length
        .as_deref()
        .map(|raw| parse_decimal(raw).map_err(|_| RangeViolation::BadContentLength))
        .transpose()
}

/// Whether the response validators are consistent with the stored identity. A response
/// without a validator is only trusted when `If-Range` proved the match server-side.
fn validators_agree(identity: &ResumeIdentity, meta: &ResponseMeta, if_range_sent: bool) -> bool {
    match identity.check(&meta.validators) {
        ValidatorCheck::Same => true,
        ValidatorCheck::Different => false,
        ValidatorCheck::Unknown => if_range_sent,
    }
}

/// Decides what to do with `meta`, the response to `req`, given the stored `identity`.
///
/// Nothing may be written unless the result is [`RangeDecision::Accept`] (only
/// `[start, end_excl)`) or [`RangeDecision::Accept200Full`] (only up to `end_excl`).
#[must_use]
pub fn decide(
    req: &RangedRequest,
    identity: &ResumeIdentity,
    meta: &ResponseMeta,
) -> RangeDecision {
    if req.range.header_value().is_none() {
        return RangeDecision::Violation(RangeViolation::InvalidRequest);
    }
    let outcome = match meta.status {
        206 => decide_206(req, identity, meta),
        200 => decide_200(req, identity, meta),
        416 => decide_416(req, identity, meta),
        status @ (304 | 412) => Err(RangeViolation::UnexpectedStatus { status }),
        status => Ok(RangeDecision::OtherStatus { status }),
    };
    outcome.unwrap_or_else(RangeDecision::Violation)
}

fn decide_206(
    req: &RangedRequest,
    identity: &ResumeIdentity,
    meta: &ResponseMeta,
) -> Result<RangeDecision, RangeViolation> {
    if has_non_identity_encoding(meta) {
        return Ok(RangeDecision::UnsupportedContentEncoding);
    }
    if is_multipart(meta) {
        return Err(RangeViolation::Multipart);
    }
    let raw = meta
        .content_range
        .as_deref()
        .ok_or(RangeViolation::MissingContentRange)?;
    let parsed = ContentRange::parse(raw).map_err(|_| RangeViolation::BadContentRange)?;
    let ContentRange::Bytes { first, last, total } = parsed else {
        return Err(RangeViolation::BadContentRange);
    };
    if first != req.range.start {
        return Err(RangeViolation::WrongStart);
    }
    if last >= req.range.end_excl {
        return Err(RangeViolation::EndBeyondRequest);
    }
    // Strict by design (pinned by `rejects_206_total_mismatch`): the reported total must
    // equal the stored one, and an unknown stored total only accepts `*`.
    if total != identity.total {
        return Err(RangeViolation::TotalMismatch);
    }
    let end_excl = last.checked_add(1).ok_or(RangeViolation::BadContentRange)?;
    if parse_content_length(meta)?.is_some_and(|len| len != end_excl - first) {
        return Err(RangeViolation::ContentLengthMismatch);
    }
    if !validators_agree(identity, meta, req.if_range_sent) {
        return Ok(RangeDecision::RemoteChanged);
    }
    Ok(RangeDecision::Accept {
        start: first,
        end_excl,
        short: end_excl < req.range.end_excl,
    })
}

fn decide_200(
    req: &RangedRequest,
    identity: &ResumeIdentity,
    meta: &ResponseMeta,
) -> Result<RangeDecision, RangeViolation> {
    if has_non_identity_encoding(meta) {
        return Ok(RangeDecision::UnsupportedContentEncoding);
    }
    let length = parse_content_length(meta)?;
    let length_differs = matches!((length, identity.total), (Some(l), Some(t)) if l != t);
    // A 200 to an If-Range request means the validator no longer matches.
    if req.if_range_sent || length_differs || !validators_agree(identity, meta, false) {
        return Ok(RangeDecision::RemoteChanged);
    }
    let first_segment_from_zero = req.segment_no == 0 && req.range.start == 0;
    if first_segment_from_zero && identity.total.is_some() && length == identity.total {
        return Ok(RangeDecision::Accept200Full {
            end_excl: req.range.end_excl,
        });
    }
    if req.has_committed_data {
        return Err(RangeViolation::RangesNoLongerSupported);
    }
    Ok(RangeDecision::RangeUnsupported)
}

fn decide_416(
    req: &RangedRequest,
    identity: &ResumeIdentity,
    meta: &ResponseMeta,
) -> Result<RangeDecision, RangeViolation> {
    let Some(raw) = meta.content_range.as_deref() else {
        return Ok(RangeDecision::Reprobe);
    };
    let parsed = ContentRange::parse(raw).map_err(|_| RangeViolation::BadContentRange)?;
    let ContentRange::Unsatisfied { total } = parsed else {
        return Err(RangeViolation::UnexpectedUnsatisfied);
    };
    let Some(stored) = identity.total else {
        return Ok(RangeDecision::Reprobe);
    };
    if total != stored {
        return Ok(RangeDecision::RemoteChanged);
    }
    if req.range.start != total {
        return Err(RangeViolation::UnexpectedUnsatisfied);
    }
    if !validators_agree(identity, meta, req.if_range_sent) {
        return Ok(RangeDecision::RemoteChanged);
    }
    Ok(RangeDecision::SegmentComplete)
}
