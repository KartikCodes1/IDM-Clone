//! Probe response classification (ARCHITECTURE 4.4 and 4.5, ADR-009).
//!
//! The probe is an optional HEAD plus a `GET Range: bytes=0-0`. The GET decides; the HEAD
//! only supplies hints and is reported when it disagrees.

use forgedm_protocol::{ResumeSupport, TransferMode};

use super::content_range::ContentRange;
use super::range_decision::{
    RangeViolation, ResponseMeta, has_non_identity_encoding, is_multipart, parse_content_length,
};
use super::validators::{ResumeIdentity, Validators};

/// Smallest total that is split into segments (8 MiB); smaller files use one range stream.
pub const SEGMENTED_MIN_TOTAL: u64 = 8 * 1024 * 1024;

/// A usable probe outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    /// How the body will be transferred.
    pub transfer_mode: TransferMode,
    /// Complete length, when known.
    pub total: Option<u64>,
    /// How reliably the download can be resumed.
    pub resume: ResumeSupport,
    /// Whether completion can be proven by length.
    pub length_verified: bool,
    /// Validators from the GET (never from the HEAD).
    pub validators: Validators,
    /// The HEAD disagreed with the GET on length or ETag (for the event log).
    pub head_conflict: bool,
}

/// What the probe responses mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeClassification {
    /// The probe succeeded.
    Ready(ProbeResult),
    /// Non-identity `Content-Encoding` (Inv-07): fail, never retried.
    UnsupportedContentEncoding,
    /// 401 or 407.
    NeedsCredentials,
    /// 403, 404 or 410: failed, or `needs_url_refresh` when re-probing with progress.
    UrlRejected {
        /// The status.
        status: u16,
    },
    /// Any other status: classify with the retry rules (ARCHITECTURE 4.10).
    OtherStatus {
        /// The status.
        status: u16,
    },
    /// The GET broke the range contract.
    Violation(RangeViolation),
}

/// The HEAD response when it is a usable 2xx; failures and 405 are inconclusive.
fn usable_head(head: Option<&ResponseMeta>) -> Option<&ResponseMeta> {
    head.filter(|h| (200..300).contains(&h.status))
}

/// The length a HEAD hints at; none when it is content-coded or malformed.
fn head_total(head: Option<&ResponseMeta>) -> Option<u64> {
    let head = usable_head(head)?;
    if has_non_identity_encoding(head) {
        return None;
    }
    parse_content_length(head).ok().flatten()
}

fn head_conflicts(head: Option<&ResponseMeta>, total: Option<u64>, get: &ResponseMeta) -> bool {
    let Some(h) = usable_head(head) else {
        return false;
    };
    let length = head_total(head).is_some_and(|t| Some(t) != total);
    let etag = matches!((&h.validators.etag, &get.validators.etag), (Some(a), Some(b)) if a != b);
    length || etag
}

fn ready(
    get: &ResponseMeta,
    transfer_mode: TransferMode,
    total: Option<u64>,
    resume: ResumeSupport,
    head: Option<&ResponseMeta>,
) -> ProbeClassification {
    ProbeClassification::Ready(ProbeResult {
        transfer_mode,
        total,
        resume,
        length_verified: total.is_some(),
        validators: get.validators.clone(),
        head_conflict: head_conflicts(head, total, get),
    })
}

/// Classifies the probe: `head` is the HEAD response if one was obtained, `get` the
/// `Range: bytes=0-0` GET. Redirects must already have been followed.
#[must_use]
pub fn classify_probe(head: Option<&ResponseMeta>, get: &ResponseMeta) -> ProbeClassification {
    match get.status {
        206 => classify_206(head, get),
        200 => classify_200(head, get),
        416 => classify_416(head, get),
        401 | 407 => ProbeClassification::NeedsCredentials,
        403 | 404 | 410 => ProbeClassification::UrlRejected { status: get.status },
        status => ProbeClassification::OtherStatus { status },
    }
}

fn classify_206(head: Option<&ResponseMeta>, get: &ResponseMeta) -> ProbeClassification {
    if has_non_identity_encoding(get) {
        return ProbeClassification::UnsupportedContentEncoding;
    }
    match first_byte_total(get) {
        Ok(total) => ranged(head, get, total),
        Err(violation) => ProbeClassification::Violation(violation),
    }
}

/// Checks the 206 is exactly `bytes 0-0/T|*` with a one-byte body; returns the total.
fn first_byte_total(get: &ResponseMeta) -> Result<Option<u64>, RangeViolation> {
    if is_multipart(get) {
        return Err(RangeViolation::Multipart);
    }
    let raw = get
        .content_range
        .as_deref()
        .ok_or(RangeViolation::MissingContentRange)?;
    let parsed = ContentRange::parse(raw).map_err(|_| RangeViolation::BadContentRange)?;
    let ContentRange::Bytes { first, last, total } = parsed else {
        return Err(RangeViolation::BadContentRange);
    };
    if first != 0 {
        return Err(RangeViolation::WrongStart);
    }
    if last != 0 {
        return Err(RangeViolation::EndBeyondRequest);
    }
    if parse_content_length(get)?.is_some_and(|len| len != 1) {
        return Err(RangeViolation::ContentLengthMismatch);
    }
    Ok(total)
}

fn ranged(
    head: Option<&ResponseMeta>,
    get: &ResponseMeta,
    total: Option<u64>,
) -> ProbeClassification {
    let strong = ResumeIdentity::from_validators(total, &get.validators).has_strong_validator();
    let (mode, resume) = match total {
        None => (
            TransferMode::SingleRangedUnknownLength,
            ResumeSupport::BestEffort,
        ),
        Some(t) => {
            let mode = if t >= SEGMENTED_MIN_TOTAL {
                TransferMode::Segmented
            } else {
                TransferMode::SingleRanged
            };
            (
                mode,
                if strong {
                    ResumeSupport::Reliable
                } else {
                    ResumeSupport::BestEffort
                },
            )
        }
    };
    ready(get, mode, total, resume, head)
}

fn classify_200(head: Option<&ResponseMeta>, get: &ResponseMeta) -> ProbeClassification {
    if has_non_identity_encoding(get) {
        return ProbeClassification::UnsupportedContentEncoding;
    }
    match parse_content_length(get) {
        Err(violation) => ProbeClassification::Violation(violation),
        Ok(Some(0)) => ready(
            get,
            TransferMode::ZeroLength,
            Some(0),
            ResumeSupport::NotResumable,
            head,
        ),
        Ok(Some(n)) => ready(
            get,
            TransferMode::SingleNonresumable,
            Some(n),
            ResumeSupport::NotResumable,
            head,
        ),
        Ok(None) => ready(
            get,
            TransferMode::SingleUnknownLength,
            None,
            ResumeSupport::NotResumable,
            head,
        ),
    }
}

fn classify_416(head: Option<&ResponseMeta>, get: &ResponseMeta) -> ProbeClassification {
    let parsed = get.content_range.as_deref().map(ContentRange::parse);
    if matches!(parsed, Some(Ok(ContentRange::Unsatisfied { total: 0 }))) {
        return ready(
            get,
            TransferMode::ZeroLength,
            Some(0),
            ResumeSupport::NotResumable,
            head,
        );
    }
    // Any other 416 to `bytes=0-0` means ranges are not honoured; only the HEAD can hint a length.
    match head_total(head).filter(|&n| n > 0) {
        Some(n) => ready(
            get,
            TransferMode::SingleNonresumable,
            Some(n),
            ResumeSupport::NotResumable,
            head,
        ),
        None => ready(
            get,
            TransferMode::SingleUnknownLength,
            None,
            ResumeSupport::NotResumable,
            head,
        ),
    }
}
