//! S1.2 range decision tests: every row of ARCHITECTURE 4.6 (THREAT_MODEL NET-01, NET-02, Inv-07).

use forgedm_core::http::content_range::RangeRequest;
use forgedm_core::http::range_decision::{
    RangeDecision, RangeViolation, RangedRequest, ResponseMeta, decide,
};
use forgedm_core::http::validators::{ResumeIdentity, Validators};
use proptest::prelude::*;

const TOTAL: u64 = 1000;

fn etag(tag: &str) -> Validators {
    Validators {
        etag: Some(tag.to_owned()),
        ..Validators::default()
    }
}

fn identity() -> ResumeIdentity {
    ResumeIdentity::from_validators(Some(TOTAL), &etag("\"v1\""))
}

fn request(start: u64, end_excl: u64) -> RangedRequest {
    RangedRequest {
        range: RangeRequest { start, end_excl },
        if_range_sent: true,
        segment_no: u32::from(start != 0),
        has_committed_data: false,
    }
}

fn r206(content_range: &str, content_length: Option<&str>) -> ResponseMeta {
    ResponseMeta {
        status: 206,
        content_range: Some(content_range.to_owned()),
        content_length: content_length.map(str::to_owned),
        validators: etag("\"v1\""),
        ..ResponseMeta::default()
    }
}

fn r200(content_length: Option<&str>, validators: Validators) -> ResponseMeta {
    ResponseMeta {
        status: 200,
        content_length: content_length.map(str::to_owned),
        validators,
        ..ResponseMeta::default()
    }
}

fn r416(content_range: Option<&str>) -> ResponseMeta {
    ResponseMeta {
        status: 416,
        content_range: content_range.map(str::to_owned),
        ..ResponseMeta::default()
    }
}

fn violation(decision: RangeDecision) -> RangeViolation {
    match decision {
        RangeDecision::Violation(v) => v,
        other => unreachable!("expected a violation, got {other:?}"),
    }
}

#[test]
fn accepts_exact_206() {
    let decision = decide(
        &request(100, 200),
        &identity(),
        &r206("bytes 100-199/1000", Some("100")),
    );
    assert_eq!(
        decision,
        RangeDecision::Accept {
            start: 100,
            end_excl: 200,
            short: false
        }
    );
    // Content-Length is optional (chunked 206).
    let no_cl = decide(
        &request(100, 200),
        &identity(),
        &r206("bytes 100-199/1000", None),
    );
    assert_eq!(
        no_cl,
        RangeDecision::Accept {
            start: 100,
            end_excl: 200,
            short: false
        }
    );
}

#[test]
fn accepts_206_with_star_total_while_unknown() {
    let unknown = ResumeIdentity::from_validators(None, &Validators::default());
    let req = RangedRequest {
        if_range_sent: false,
        ..request(0, 10)
    };
    let meta = ResponseMeta {
        validators: Validators::default(),
        ..r206("bytes 0-9/*", Some("10"))
    };
    assert_eq!(
        decide(&req, &unknown, &meta),
        RangeDecision::Accept {
            start: 0,
            end_excl: 10,
            short: false
        }
    );
}

#[test]
fn _416_without_if_range_and_strong_validator_is_remote_changed() {
    // Pins the documented invariant: a validator-less 416 proves nothing without If-Range.
    let req = RangedRequest {
        if_range_sent: false,
        ..request(1000, 1001)
    };
    assert_eq!(
        decide(&req, &identity(), &r416(Some("bytes */1000"))),
        RangeDecision::RemoteChanged
    );
}

#[test]
fn accepts_short_206_with_continuation() {
    let decision = decide(
        &request(100, 200),
        &identity(),
        &r206("bytes 100-149/1000", Some("50")),
    );
    assert_eq!(
        decision,
        RangeDecision::Accept {
            start: 100,
            end_excl: 150,
            short: true
        }
    );
}

#[test]
fn rejects_206_wrong_start() {
    for cr in ["bytes 0-99/1000", "bytes 101-199/1000", "bytes 99-199/1000"] {
        assert_eq!(
            violation(decide(&request(100, 200), &identity(), &r206(cr, None))),
            RangeViolation::WrongStart,
            "{cr}"
        );
    }
}

#[test]
fn rejects_206_end_beyond_request() {
    // x == e (one byte past the request) and far past are both violations.
    for cr in ["bytes 100-200/1000", "bytes 100-999/1000"] {
        assert_eq!(
            violation(decide(&request(100, 200), &identity(), &r206(cr, None))),
            RangeViolation::EndBeyondRequest,
            "{cr}"
        );
    }
}

#[test]
fn rejects_206_total_mismatch() {
    assert_eq!(
        violation(decide(
            &request(100, 200),
            &identity(),
            &r206("bytes 100-199/2000", None)
        )),
        RangeViolation::TotalMismatch
    );
    // A known total never accepts `*`.
    assert_eq!(
        violation(decide(
            &request(100, 200),
            &identity(),
            &r206("bytes 100-199/*", None)
        )),
        RangeViolation::TotalMismatch
    );
    // An unknown stored total only accepts `*`.
    let unknown = ResumeIdentity::from_validators(None, &Validators::default());
    assert_eq!(
        violation(decide(
            &request(100, 200),
            &unknown,
            &r206("bytes 100-199/1000", None)
        )),
        RangeViolation::TotalMismatch
    );
}

#[test]
fn rejects_206_bad_content_range_forms() {
    let id = identity();
    let req = request(100, 200);
    for (cr, expected) in [
        ("bytes 199-100/1000", RangeViolation::BadContentRange),
        ("bytes 100-1000/1000", RangeViolation::BadContentRange),
        ("items 100-199/1000", RangeViolation::BadContentRange),
        (
            "bytes 100-199/99999999999999999999999",
            RangeViolation::BadContentRange,
        ),
        ("garbage", RangeViolation::BadContentRange),
        ("bytes */1000", RangeViolation::BadContentRange),
    ] {
        assert_eq!(
            violation(decide(&req, &id, &r206(cr, None))),
            expected,
            "{cr}"
        );
    }
    let missing = ResponseMeta {
        status: 206,
        validators: etag("\"v1\""),
        ..ResponseMeta::default()
    };
    assert_eq!(
        violation(decide(&req, &id, &missing)),
        RangeViolation::MissingContentRange
    );
}

#[test]
fn rejects_multipart_206() {
    let meta = ResponseMeta {
        status: 206,
        content_type: Some("multipart/byteranges; boundary=THIS_STRING_SEPARATES".to_owned()),
        validators: etag("\"v1\""),
        ..ResponseMeta::default()
    };
    assert_eq!(
        violation(decide(&request(0, 200), &identity(), &meta)),
        RangeViolation::Multipart
    );
    // Multipart is rejected even when a Content-Range is also present, in any case.
    let with_cr = ResponseMeta {
        content_type: Some("Multipart/ByteRanges; boundary=x".to_owned()),
        content_range: Some("bytes 0-199/1000".to_owned()),
        ..meta
    };
    assert_eq!(
        violation(decide(&request(0, 200), &identity(), &with_cr)),
        RangeViolation::Multipart
    );
}

#[test]
fn rejects_content_encoded_206() {
    for encoding in [
        "gzip",
        "br",
        "deflate",
        "zstd",
        "GZIP",
        "gzip, identity",
        "identity, gzip",
        "x-foo",
    ] {
        let meta = ResponseMeta {
            content_encoding: Some(encoding.to_owned()),
            ..r206("bytes 100-199/1000", Some("100"))
        };
        assert_eq!(
            decide(&request(100, 200), &identity(), &meta),
            RangeDecision::UnsupportedContentEncoding,
            "{encoding}"
        );
    }
    // `identity` (any case, padded) is not an encoding.
    for ok in ["identity", "IDENTITY", " identity "] {
        let meta = ResponseMeta {
            content_encoding: Some(ok.to_owned()),
            ..r206("bytes 100-199/1000", Some("100"))
        };
        assert!(matches!(
            decide(&request(100, 200), &identity(), &meta),
            RangeDecision::Accept { .. }
        ));
    }
}

#[test]
fn rejects_content_encoded_200() {
    let meta = ResponseMeta {
        content_encoding: Some("gzip".to_owned()),
        ..r200(Some("1000"), etag("\"v1\""))
    };
    let req = RangedRequest {
        if_range_sent: false,
        ..request(0, 1000)
    };
    assert_eq!(
        decide(&req, &identity(), &meta),
        RangeDecision::UnsupportedContentEncoding
    );
}

#[test]
fn rejects_206_cl_mismatch() {
    for cl in ["99", "101", "0", "1000"] {
        assert_eq!(
            violation(decide(
                &request(100, 200),
                &identity(),
                &r206("bytes 100-199/1000", Some(cl))
            )),
            RangeViolation::ContentLengthMismatch,
            "CL {cl}"
        );
    }
    // Short 206: Content-Length must match the shortened window.
    assert_eq!(
        violation(decide(
            &request(100, 200),
            &identity(),
            &r206("bytes 100-149/1000", Some("100"))
        )),
        RangeViolation::ContentLengthMismatch
    );
    for bad in ["abc", "-1", "1e2", "100 ", "999999999999999999999"] {
        assert_eq!(
            violation(decide(
                &request(100, 200),
                &identity(),
                &r206("bytes 100-199/1000", Some(bad))
            )),
            RangeViolation::BadContentLength,
            "CL {bad:?}"
        );
    }
}

#[test]
fn rejects_206_changed_validators() {
    let other = ResponseMeta {
        validators: etag("\"v2\""),
        ..r206("bytes 100-199/1000", None)
    };
    assert_eq!(
        decide(&request(100, 200), &identity(), &other),
        RangeDecision::RemoteChanged
    );
    let weak = ResponseMeta {
        validators: etag("W/\"v1\""),
        ..r206("bytes 100-199/1000", None)
    };
    assert_eq!(
        decide(&request(100, 200), &identity(), &weak),
        RangeDecision::RemoteChanged
    );
    // No validator on the response: acceptable only because If-Range proved the match.
    let bare = ResponseMeta {
        validators: Validators::default(),
        ..r206("bytes 100-199/1000", None)
    };
    assert!(matches!(
        decide(&request(100, 200), &identity(), &bare),
        RangeDecision::Accept { .. }
    ));
    let no_if_range = RangedRequest {
        if_range_sent: false,
        ..request(100, 200)
    };
    assert_eq!(
        decide(&no_if_range, &identity(), &bare),
        RangeDecision::RemoteChanged
    );
}

#[test]
fn rejects_invalid_request_range() {
    let meta = r206("bytes 5-5/1000", None);
    assert_eq!(
        violation(decide(&request(5, 5), &identity(), &meta)),
        RangeViolation::InvalidRequest
    );
}

#[test]
fn _200_to_ranged_with_if_range_is_remote_changed() {
    // The server would have sent 206 had the validator still matched.
    let req = request(100, 200);
    let meta = r200(Some("1000"), etag("\"v1\""));
    assert_eq!(
        decide(&req, &identity(), &meta),
        RangeDecision::RemoteChanged
    );
    let seg0 = RangedRequest {
        segment_no: 0,
        ..request(0, 200)
    };
    assert_eq!(
        decide(&seg0, &identity(), &meta),
        RangeDecision::RemoteChanged
    );
}

#[test]
fn _200_same_validators_is_range_unsupported() {
    let req = RangedRequest {
        if_range_sent: false,
        ..request(100, 200)
    };
    let meta = r200(Some("1000"), etag("\"v1\""));
    // Nothing committed: re-plan as single_nonresumable.
    assert_eq!(
        decide(&req, &identity(), &meta),
        RangeDecision::RangeUnsupported
    );
    // Something committed: protocol violation.
    let committed = RangedRequest {
        has_committed_data: true,
        ..req
    };
    assert_eq!(
        violation(decide(&committed, &identity(), &meta)),
        RangeViolation::RangesNoLongerSupported
    );
    // Different validators or a different length: the file changed.
    let changed = r200(Some("1000"), etag("\"v2\""));
    assert_eq!(
        decide(&req, &identity(), &changed),
        RangeDecision::RemoteChanged
    );
    let resized = r200(Some("999"), etag("\"v1\""));
    assert_eq!(
        decide(&req, &identity(), &resized),
        RangeDecision::RemoteChanged
    );
    // A strong identity with a bare 200 cannot prove sameness.
    let bare = r200(Some("1000"), Validators::default());
    assert_eq!(
        decide(&req, &identity(), &bare),
        RangeDecision::RemoteChanged
    );
}

#[test]
fn _200_segment0_full_allowed_only_s0_and_cl_eq_total() {
    let seg0 = RangedRequest {
        if_range_sent: false,
        segment_no: 0,
        ..request(0, 250)
    };
    let same = r200(Some("1000"), etag("\"v1\""));
    // May read up to the end of the segment's request window.
    assert_eq!(
        decide(&seg0, &identity(), &same),
        RangeDecision::Accept200Full { end_excl: 250 }
    );
    // Not for another segment, even when the offset happens to be 0.
    let seg1 = RangedRequest {
        segment_no: 1,
        ..seg0
    };
    assert_eq!(
        decide(&seg1, &identity(), &same),
        RangeDecision::RangeUnsupported
    );
    // Not at a non-zero start.
    let resumed = RangedRequest {
        range: RangeRequest {
            start: 10,
            end_excl: 250,
        },
        ..seg0
    };
    assert_eq!(
        decide(&resumed, &identity(), &same),
        RangeDecision::RangeUnsupported
    );
    // Not when Content-Length is absent: the length is unproven.
    let no_cl = r200(None, etag("\"v1\""));
    assert_eq!(
        decide(&seg0, &identity(), &no_cl),
        RangeDecision::RangeUnsupported
    );
    // A different Content-Length is a change.
    let other = r200(Some("1001"), etag("\"v1\""));
    assert_eq!(
        decide(&seg0, &identity(), &other),
        RangeDecision::RemoteChanged
    );
    // Not when the total is unknown.
    let unknown = ResumeIdentity::from_validators(None, &etag("\"v1\""));
    assert_eq!(
        decide(&seg0, &unknown, &same),
        RangeDecision::RangeUnsupported
    );
    // Not when Content-Length is not a number.
    let bad = r200(Some("x"), etag("\"v1\""));
    assert_eq!(
        violation(decide(&seg0, &identity(), &bad)),
        RangeViolation::BadContentLength
    );
}

#[test]
fn _416_at_end_matching_total_is_complete() {
    // With a strong identity the request carried If-Range, so a validator-less 416 is trusted.
    let proven = request(1000, 1001);
    assert_eq!(
        decide(&proven, &identity(), &r416(Some("bytes */1000"))),
        RangeDecision::SegmentComplete
    );
    let req = RangedRequest {
        if_range_sent: false,
        ..proven
    };
    // A differing validator on the 416 is a change.
    let changed = ResponseMeta {
        validators: etag("\"v2\""),
        ..r416(Some("bytes */1000"))
    };
    assert_eq!(
        decide(&req, &identity(), &changed),
        RangeDecision::RemoteChanged
    );
    // Same validator is fine.
    let same = ResponseMeta {
        validators: etag("\"v1\""),
        ..r416(Some("bytes */1000"))
    };
    assert_eq!(
        decide(&req, &identity(), &same),
        RangeDecision::SegmentComplete
    );
    // A strong identity with no validator on a 416 and no If-Range proves nothing.
    let bare = r416(Some("bytes */1000"));
    assert_eq!(
        decide(&req, &identity(), &bare),
        RangeDecision::RemoteChanged
    );
}

#[test]
fn _416_other_total_remote_changed() {
    let req = RangedRequest {
        if_range_sent: false,
        ..request(1000, 1001)
    };
    assert_eq!(
        decide(&req, &identity(), &r416(Some("bytes */2000"))),
        RangeDecision::RemoteChanged
    );
    assert_eq!(
        decide(&req, &identity(), &r416(Some("bytes */0"))),
        RangeDecision::RemoteChanged
    );
    // Start before the total with matching total is not a completed segment.
    let early = RangedRequest {
        if_range_sent: false,
        ..request(500, 600)
    };
    assert_eq!(
        violation(decide(&early, &identity(), &r416(Some("bytes */1000")))),
        RangeViolation::UnexpectedUnsatisfied
    );
    // An unknown stored total cannot confirm completion.
    let unknown = ResumeIdentity::from_validators(None, &Validators::default());
    assert_eq!(
        decide(&req, &unknown, &r416(Some("bytes */1000"))),
        RangeDecision::Reprobe
    );
    // A satisfied-range form on a 416 is malformed.
    assert_eq!(
        violation(decide(&req, &identity(), &r416(Some("bytes 0-9/1000")))),
        RangeViolation::UnexpectedUnsatisfied
    );
    assert_eq!(
        violation(decide(&req, &identity(), &r416(Some("junk")))),
        RangeViolation::BadContentRange
    );
}

#[test]
fn _416_no_cr_reprobe() {
    let req = request(1000, 1001);
    assert_eq!(
        decide(&req, &identity(), &r416(None)),
        RangeDecision::Reprobe
    );
}

#[test]
fn _304_412_protocol_error() {
    for status in [304, 412] {
        let meta = ResponseMeta {
            status,
            ..ResponseMeta::default()
        };
        assert_eq!(
            violation(decide(&request(0, 10), &identity(), &meta)),
            RangeViolation::UnexpectedStatus { status }
        );
    }
}

#[test]
fn other_statuses_are_routed_not_judged() {
    for status in [204, 301, 401, 403, 404, 429, 500, 503] {
        let meta = ResponseMeta {
            status,
            ..ResponseMeta::default()
        };
        assert_eq!(
            decide(&request(0, 10), &identity(), &meta),
            RangeDecision::OtherStatus { status }
        );
    }
}

#[test]
fn violation_reasons_are_stable_machine_strings() {
    assert_eq!(RangeViolation::WrongStart.reason(), "wrong_start");
    assert_eq!(
        RangeViolation::RangesNoLongerSupported.reason(),
        "ranges_no_longer_supported"
    );
    assert_eq!(RangeViolation::Multipart.reason(), "multipart");
    assert_eq!(
        RangeViolation::UnexpectedStatus { status: 304 }.reason(),
        "unexpected_status"
    );
}

proptest! {
    #[test]
    fn proptest_accept_window_within_request(
        start in 0u64..2000,
        len in 0u64..2000,
        first in 0u64..2200,
        last in 0u64..2200,
        total in prop::option::of(0u64..2200),
        stored in prop::option::of(0u64..2200),
        cl in prop::option::of(0u64..2200),
        status in prop::sample::select(vec![200u16, 206, 416, 304]),
        segment_no in 0u32..3,
        if_range_sent in any::<bool>(),
        committed in any::<bool>(),
    ) {
        let req = RangedRequest {
            range: RangeRequest { start, end_excl: start + len },
            if_range_sent,
            segment_no,
            has_committed_data: committed,
        };
        let content_range = match total {
            Some(t) => format!("bytes {first}-{last}/{t}"),
            None => format!("bytes {first}-{last}/*"),
        };
        let meta = ResponseMeta {
            status,
            content_range: Some(content_range),
            content_length: cl.map(|c| c.to_string()),
            validators: etag("\"v1\""),
            ..ResponseMeta::default()
        };
        let id = ResumeIdentity::from_validators(stored, &etag("\"v1\""));
        match decide(&req, &id, &meta) {
            RangeDecision::Accept { start: s, end_excl: e, short } => {
                prop_assert_eq!(s, start);
                prop_assert!(e > s);
                prop_assert!(e <= req.range.end_excl);
                prop_assert_eq!(short, e < req.range.end_excl);
                prop_assert_eq!(status, 206);
            }
            RangeDecision::Accept200Full { end_excl } => {
                prop_assert_eq!(end_excl, req.range.end_excl);
                prop_assert_eq!(start, 0);
                prop_assert_eq!(segment_no, 0);
                prop_assert_eq!(status, 200);
            }
            _ => {}
        }
    }
}
