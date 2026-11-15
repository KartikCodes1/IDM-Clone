//! S1.2 Content-Range parser and request-range tests (THREAT_MODEL NET-01).

use forgedm_core::http::content_range::{ContentRange, ContentRangeError, RangeRequest};
use proptest::prelude::*;

#[test]
fn parses_total_and_star() {
    assert_eq!(
        ContentRange::parse("bytes 0-499/1234"),
        Ok(ContentRange::Bytes {
            first: 0,
            last: 499,
            total: Some(1234)
        })
    );
    assert_eq!(
        ContentRange::parse("bytes 0-0/*"),
        Ok(ContentRange::Bytes {
            first: 0,
            last: 0,
            total: None
        })
    );
    assert_eq!(
        ContentRange::parse("bytes */1234"),
        Ok(ContentRange::Unsatisfied { total: 1234 })
    );
    assert_eq!(
        ContentRange::parse("bytes */0"),
        Ok(ContentRange::Unsatisfied { total: 0 })
    );
    // The unit is case-insensitive and surrounding whitespace is optional whitespace.
    assert_eq!(
        ContentRange::parse("  BYTES 5-9/10 \t"),
        Ok(ContentRange::Bytes {
            first: 5,
            last: 9,
            total: Some(10)
        })
    );
    // The last byte of the representation.
    assert_eq!(
        ContentRange::parse("bytes 9-9/10"),
        Ok(ContentRange::Bytes {
            first: 9,
            last: 9,
            total: Some(10)
        })
    );
}

#[test]
fn rejects_inverted() {
    assert_eq!(
        ContentRange::parse("bytes 10-9/100"),
        Err(ContentRangeError::Inverted)
    );
    assert_eq!(
        ContentRange::parse("bytes 500-1/*"),
        Err(ContentRangeError::Inverted)
    );
}

#[test]
fn rejects_last_ge_total() {
    assert_eq!(
        ContentRange::parse("bytes 0-100/100"),
        Err(ContentRangeError::LastNotBelowTotal)
    );
    assert_eq!(
        ContentRange::parse("bytes 0-101/100"),
        Err(ContentRangeError::LastNotBelowTotal)
    );
    assert_eq!(
        ContentRange::parse("bytes 0-0/0"),
        Err(ContentRangeError::LastNotBelowTotal)
    );
}

#[test]
fn rejects_non_bytes_unit() {
    for bad in ["items 0-9/10", "bits 0-9/10", "byte 0-9/10", "0-9/10"] {
        assert!(ContentRange::parse(bad).is_err(), "{bad} must be rejected");
    }
    assert_eq!(
        ContentRange::parse("items 0-9/10"),
        Err(ContentRangeError::UnitNotBytes)
    );
}

#[test]
fn rejects_garbage() {
    for bad in [
        "",
        " ",
        "bytes",
        "bytes ",
        "bytes 0-9",
        "bytes 0-9/",
        "bytes /10",
        "bytes -9/10",
        "bytes 0-/10",
        "bytes */*",
        "bytes *-5/10",
        "bytes 0-9/10 extra",
        "bytes 0-9/10, bytes 11-12/20",
        "bytes  0-9/10",
        "bytes 0 - 9/10",
        "bytes +1-9/10",
        "bytes 0x1-9/10",
        "bytes 1.5-9/10",
        "bytes 0-9/1e3",
        "bytes \u{0661}-9/10",
        "bytes 0-9/10\r\n",
    ] {
        assert!(
            ContentRange::parse(bad).is_err(),
            "{bad:?} must be rejected"
        );
    }
}

#[test]
fn rejects_u64_overflow_accepts_above_u32() {
    // Above u32::MAX but within u64 is fine.
    assert_eq!(
        ContentRange::parse("bytes 4294967296-5000000000/9000000000"),
        Ok(ContentRange::Bytes {
            first: 4_294_967_296,
            last: 5_000_000_000,
            total: Some(9_000_000_000)
        })
    );
    // u64::MAX itself has 20 digits and is accepted as a total.
    assert_eq!(
        ContentRange::parse("bytes 0-1/18446744073709551615"),
        Ok(ContentRange::Bytes {
            first: 0,
            last: 1,
            total: Some(u64::MAX)
        })
    );
    // u64::MAX + 1 is 20 digits and overflows.
    assert_eq!(
        ContentRange::parse("bytes 0-1/18446744073709551616"),
        Err(ContentRangeError::Overflow)
    );
    assert_eq!(
        ContentRange::parse("bytes 18446744073709551616-18446744073709551617/*"),
        Err(ContentRangeError::Overflow)
    );
    // More than 20 digits is rejected on length alone, even with leading zeros.
    assert_eq!(
        ContentRange::parse("bytes 0-1/000000000000000000001"),
        Err(ContentRangeError::TooManyDigits)
    );
    assert_eq!(
        ContentRange::parse("bytes */123456789012345678901"),
        Err(ContentRangeError::TooManyDigits)
    );
}

proptest! {
    #[test]
    fn proptest_roundtrip_never_panics(
        first in 0u64..u64::MAX - 2,
        span in 0u64..1_000_000,
        slack in 1u64..1_000_000,
        unknown_total in any::<bool>(),
        noise in ".{0,64}",
    ) {
        let last = first.saturating_add(span).min(u64::MAX - 2);
        let total = last + slack.min(u64::MAX - last);
        let rendered = if unknown_total {
            format!("bytes {first}-{last}/*")
        } else {
            format!("bytes {first}-{last}/{total}")
        };
        let parsed = ContentRange::parse(&rendered);
        prop_assert_eq!(
            parsed,
            Ok(ContentRange::Bytes { first, last, total: (!unknown_total).then_some(total) })
        );
        // Arbitrary text never panics.
        let _ = ContentRange::parse(&noise);
    }
}

#[test]
fn range_request_header_value_is_closed_range() {
    let req = RangeRequest {
        start: 100,
        end_excl: 200,
    };
    assert_eq!(req.header_value().as_deref(), Some("bytes=100-199"));
    let one = RangeRequest {
        start: 0,
        end_excl: 1,
    };
    assert_eq!(one.header_value().as_deref(), Some("bytes=0-0"));
    let big = RangeRequest {
        start: 4_294_967_296,
        end_excl: u64::MAX,
    };
    assert_eq!(
        big.header_value().as_deref(),
        Some("bytes=4294967296-18446744073709551614")
    );
}

#[test]
fn range_request_empty_or_inverted_has_no_header() {
    assert_eq!(
        RangeRequest {
            start: 5,
            end_excl: 5
        }
        .header_value(),
        None
    );
    assert_eq!(
        RangeRequest {
            start: 6,
            end_excl: 5
        }
        .header_value(),
        None
    );
    assert_eq!(
        RangeRequest {
            start: 0,
            end_excl: 0
        }
        .header_value(),
        None
    );
}
