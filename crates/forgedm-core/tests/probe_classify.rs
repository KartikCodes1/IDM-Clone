//! S1.2 probe classification tests (ARCHITECTURE 4.4, 4.5; Inv-07).

use forgedm_core::http::probe_classify::{ProbeClassification, ProbeResult, classify_probe};
use forgedm_core::http::range_decision::{RangeViolation, ResponseMeta};
use forgedm_core::http::validators::Validators;
use forgedm_protocol::{ResumeSupport, TransferMode};

const MIB: u64 = 1024 * 1024;

fn etag(tag: &str) -> Validators {
    Validators {
        etag: Some(tag.to_owned()),
        ..Validators::default()
    }
}

fn head(content_length: &str, validators: Validators) -> ResponseMeta {
    ResponseMeta {
        status: 200,
        content_length: Some(content_length.to_owned()),
        validators,
        ..ResponseMeta::default()
    }
}

fn get206(
    content_range: &str,
    content_length: Option<&str>,
    validators: Validators,
) -> ResponseMeta {
    ResponseMeta {
        status: 206,
        content_range: Some(content_range.to_owned()),
        content_length: content_length.map(str::to_owned),
        validators,
        ..ResponseMeta::default()
    }
}

fn get200(content_length: Option<&str>) -> ResponseMeta {
    ResponseMeta {
        status: 200,
        content_length: content_length.map(str::to_owned),
        ..ResponseMeta::default()
    }
}

fn status(code: u16) -> ResponseMeta {
    ResponseMeta {
        status: code,
        ..ResponseMeta::default()
    }
}

fn ready(c: ProbeClassification) -> ProbeResult {
    match c {
        ProbeClassification::Ready(r) => r,
        other => unreachable!("expected Ready, got {other:?}"),
    }
}

#[test]
fn get_overrides_head() {
    let h = head("5000", etag("\"head\""));
    let g = get206("bytes 0-0/20000000", Some("1"), etag("\"get\""));
    let r = ready(classify_probe(Some(&h), &g));
    assert_eq!(r.total, Some(20_000_000));
    assert_eq!(
        r.validators.etag.as_deref(),
        Some("\"get\""),
        "validators come from the GET"
    );
    assert_eq!(r.transfer_mode, TransferMode::Segmented);
    assert!(
        r.head_conflict,
        "a HEAD/GET disagreement is reported for the event log"
    );

    // Agreement is not a conflict.
    let agree = head("20000000", etag("\"get\""));
    assert!(!ready(classify_probe(Some(&agree), &g)).head_conflict);
    // No HEAD is not a conflict either.
    assert!(!ready(classify_probe(None, &g)).head_conflict);
    // A HEAD that failed (405) or is content-coded is ignored, not a conflict.
    assert!(!ready(classify_probe(Some(&status(405)), &g)).head_conflict);
    let coded = ResponseMeta {
        content_encoding: Some("gzip".to_owned()),
        ..head("77", Validators::default())
    };
    assert!(!ready(classify_probe(Some(&coded), &g)).head_conflict);
}

#[test]
fn ranged_modes_by_size_and_validator() {
    let strong = etag("\"v\"");
    let big = ready(classify_probe(
        None,
        &get206("bytes 0-0/8388608", Some("1"), strong.clone()),
    ));
    assert_eq!(
        (big.transfer_mode, big.resume, big.length_verified),
        (TransferMode::Segmented, ResumeSupport::Reliable, true)
    );
    let edge = ready(classify_probe(
        None,
        &get206(
            &format!("bytes 0-0/{}", 8 * MIB - 1),
            Some("1"),
            strong.clone(),
        ),
    ));
    assert_eq!(edge.transfer_mode, TransferMode::SingleRanged);
    assert_eq!(edge.resume, ResumeSupport::Reliable);
    let weak = ready(classify_probe(
        None,
        &get206("bytes 0-0/100", Some("1"), etag("W/\"v\"")),
    ));
    assert_eq!(
        (weak.transfer_mode, weak.resume),
        (TransferMode::SingleRanged, ResumeSupport::BestEffort)
    );
    let none = ready(classify_probe(
        None,
        &get206("bytes 0-0/9999999999", None, Validators::default()),
    ));
    assert_eq!(
        (none.transfer_mode, none.resume, none.total),
        (
            TransferMode::Segmented,
            ResumeSupport::BestEffort,
            Some(9_999_999_999)
        )
    );
    let one = ready(classify_probe(
        None,
        &get206("bytes 0-0/1", Some("1"), strong),
    ));
    assert_eq!(one.transfer_mode, TransferMode::SingleRanged);
}

#[test]
fn zero_byte_via_416_star0_and_200_cl0() {
    let via_416 = ResponseMeta {
        content_range: Some("bytes */0".to_owned()),
        ..status(416)
    };
    let r = ready(classify_probe(None, &via_416));
    assert_eq!(
        (r.transfer_mode, r.total, r.length_verified),
        (TransferMode::ZeroLength, Some(0), true)
    );
    let via_200 = ready(classify_probe(None, &get200(Some("0"))));
    assert_eq!(
        (via_200.transfer_mode, via_200.total),
        (TransferMode::ZeroLength, Some(0))
    );
    // A 416 for a non-empty file does not claim zero length.
    let other_416 = ResponseMeta {
        content_range: Some("bytes */500".to_owned()),
        ..status(416)
    };
    let r = ready(classify_probe(None, &other_416));
    assert_ne!(r.transfer_mode, TransferMode::ZeroLength);
    assert_eq!(r.resume, ResumeSupport::NotResumable);
    // 416 with no Content-Range is likewise "ranges ignored".
    let bare_416 = ready(classify_probe(None, &status(416)));
    assert_eq!(bare_416.resume, ResumeSupport::NotResumable);
    assert_ne!(bare_416.transfer_mode, TransferMode::ZeroLength);
}

#[test]
fn no_range_when_get_200() {
    let with_cl = ready(classify_probe(None, &get200(Some("123456"))));
    assert_eq!(with_cl.transfer_mode, TransferMode::SingleNonresumable);
    assert_eq!(with_cl.resume, ResumeSupport::NotResumable);
    assert_eq!(
        (with_cl.total, with_cl.length_verified),
        (Some(123_456), true)
    );

    let without = ready(classify_probe(None, &get200(None)));
    assert_eq!(without.transfer_mode, TransferMode::SingleUnknownLength);
    assert_eq!(without.resume, ResumeSupport::NotResumable);
    assert_eq!((without.total, without.length_verified), (None, false));
    // The HEAD length is a hint only; the GET (no length) wins.
    let h = head("5000", Validators::default());
    assert_eq!(ready(classify_probe(Some(&h), &get200(None))).total, None);
    // A malformed Content-Length on the GET is not trusted.
    assert!(matches!(
        classify_probe(None, &get200(Some("12abc"))),
        ProbeClassification::Violation(RangeViolation::BadContentLength)
    ));
}

#[test]
fn unknown_length() {
    let r = ready(classify_probe(
        None,
        &get206("bytes 0-0/*", Some("1"), etag("\"v\"")),
    ));
    assert_eq!(r.transfer_mode, TransferMode::SingleRangedUnknownLength);
    assert_eq!(r.resume, ResumeSupport::BestEffort);
    assert_eq!((r.total, r.length_verified), (None, false));
}

#[test]
fn content_encoded_200_rejected() {
    let coded = ResponseMeta {
        content_encoding: Some("gzip".to_owned()),
        ..get200(Some("10"))
    };
    assert_eq!(
        classify_probe(None, &coded),
        ProbeClassification::UnsupportedContentEncoding
    );
    let coded206 = ResponseMeta {
        content_encoding: Some("br".to_owned()),
        ..get206("bytes 0-0/10", Some("1"), Validators::default())
    };
    assert_eq!(
        classify_probe(None, &coded206),
        ProbeClassification::UnsupportedContentEncoding
    );
    let identity = ResponseMeta {
        content_encoding: Some("identity".to_owned()),
        ..get200(Some("10"))
    };
    assert!(matches!(
        classify_probe(None, &identity),
        ProbeClassification::Ready(_)
    ));
}

#[test]
fn probe_206_must_be_exactly_the_first_byte() {
    let bad =
        |cr: &str, cl: Option<&str>| classify_probe(None, &get206(cr, cl, Validators::default()));
    for (cr, cl) in [
        ("bytes 0-9/100", Some("10")),
        ("bytes 1-1/100", Some("1")),
        ("bytes 0-0/100", Some("2")),
        ("bytes 0-0/100", Some("0")),
        ("bytes */100", Some("1")),
        ("bytes 0-0/xyz", Some("1")),
    ] {
        assert!(
            matches!(bad(cr, cl), ProbeClassification::Violation(_)),
            "{cr} {cl:?}"
        );
    }
    let missing = ResponseMeta {
        status: 206,
        ..ResponseMeta::default()
    };
    assert!(matches!(
        classify_probe(None, &missing),
        ProbeClassification::Violation(RangeViolation::MissingContentRange)
    ));
    let multipart = ResponseMeta {
        status: 206,
        content_type: Some("multipart/byteranges; boundary=b".to_owned()),
        ..ResponseMeta::default()
    };
    assert!(matches!(
        classify_probe(None, &multipart),
        ProbeClassification::Violation(RangeViolation::Multipart)
    ));
}

#[test]
fn error_statuses_are_routed() {
    for code in [401, 407] {
        assert_eq!(
            classify_probe(None, &status(code)),
            ProbeClassification::NeedsCredentials
        );
    }
    for code in [403, 404, 410] {
        assert_eq!(
            classify_probe(None, &status(code)),
            ProbeClassification::UrlRejected { status: code }
        );
    }
    for code in [400, 408, 429, 500, 502, 503, 504, 301, 304] {
        assert_eq!(
            classify_probe(None, &status(code)),
            ProbeClassification::OtherStatus { status: code }
        );
    }
}
