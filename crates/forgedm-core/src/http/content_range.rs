//! Content-Range parsing and request-range formatting (RFC 9110 section 14).
//!
//! All numbers are checked `u64`; a numeric field longer than [`MAX_DIGITS`] characters is
//! rejected on length alone so hostile input cannot force unbounded work (THREAT_MODEL NET-01).

use thiserror::Error;

/// Longest accepted decimal field: `u64::MAX` has 20 digits.
pub const MAX_DIGITS: usize = 20;

/// Why a Content-Range (or other decimal) value was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ContentRangeError {
    /// Not of the form `bytes a-b/T`, `bytes a-b/*` or `bytes */T`.
    #[error("malformed content-range syntax")]
    Syntax,
    /// The range unit is not `bytes`.
    #[error("content-range unit is not bytes")]
    UnitNotBytes,
    /// A numeric field has more than 20 characters.
    #[error("numeric field longer than 20 digits")]
    TooManyDigits,
    /// A numeric field does not fit in `u64`.
    #[error("numeric field overflows u64")]
    Overflow,
    /// First byte position is greater than the last.
    #[error("first byte position exceeds last")]
    Inverted,
    /// The last byte position is not below the complete length.
    #[error("last byte position is not below the total")]
    LastNotBelowTotal,
}

/// A parsed `Content-Range` header value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentRange {
    /// `bytes first-last/total` (or `/*` when the total is unknown). Inclusive positions.
    Bytes {
        /// First byte position.
        first: u64,
        /// Last byte position (inclusive).
        last: u64,
        /// Complete length, `None` for `*`.
        total: Option<u64>,
    },
    /// `bytes */total`, as sent with 416.
    Unsatisfied {
        /// Complete length of the representation.
        total: u64,
    },
}

/// Parses an unsigned decimal of at most [`MAX_DIGITS`] ASCII digits into a `u64`.
///
/// # Errors
/// [`ContentRangeError::Syntax`] for empty or non-digit input,
/// [`ContentRangeError::TooManyDigits`] beyond 20 characters and
/// [`ContentRangeError::Overflow`] when the value exceeds `u64::MAX`.
pub fn parse_decimal(field: &str) -> Result<u64, ContentRangeError> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ContentRangeError::Syntax);
    }
    if field.len() > MAX_DIGITS {
        return Err(ContentRangeError::TooManyDigits);
    }
    field.bytes().try_fold(0u64, |acc, digit| {
        acc.checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(digit - b'0')))
            .ok_or(ContentRangeError::Overflow)
    })
}

impl ContentRange {
    /// Parses a Content-Range value.
    ///
    /// The unit is matched case-insensitively; leading and trailing spaces and tabs are
    /// ignored. A satisfied range must have `first <= last < total`.
    ///
    /// # Errors
    /// A [`ContentRangeError`] describing the first defect found.
    pub fn parse(input: &str) -> Result<Self, ContentRangeError> {
        let value = input.trim_matches([' ', '\t']);
        let (unit, spec) = value.split_once(' ').ok_or(ContentRangeError::Syntax)?;
        if !unit.eq_ignore_ascii_case("bytes") {
            return Err(ContentRangeError::UnitNotBytes);
        }
        let (range, total) = spec.split_once('/').ok_or(ContentRangeError::Syntax)?;
        if range == "*" {
            return parse_decimal(total).map(|total| Self::Unsatisfied { total });
        }
        let (first, last) = range.split_once('-').ok_or(ContentRangeError::Syntax)?;
        let (first, last) = (parse_decimal(first)?, parse_decimal(last)?);
        let total = if total == "*" {
            None
        } else {
            Some(parse_decimal(total)?)
        };
        if first > last {
            return Err(ContentRangeError::Inverted);
        }
        if total.is_some_and(|t| last >= t) {
            return Err(ContentRangeError::LastNotBelowTotal);
        }
        Ok(Self::Bytes { first, last, total })
    }
}

/// A byte window requested from the server: `[start, end_excl)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeRequest {
    /// First requested byte.
    pub start: u64,
    /// One past the last requested byte.
    pub end_excl: u64,
}

impl RangeRequest {
    /// The `Range` header value, `bytes=start-(end_excl-1)`. This is the only place the
    /// exclusive end is converted to HTTP's inclusive form. `None` for an empty or inverted
    /// window.
    #[must_use]
    pub fn header_value(&self) -> Option<String> {
        let last = self.end_excl.checked_sub(1)?;
        (self.start <= last).then(|| format!("bytes={}-{last}", self.start))
    }
}
