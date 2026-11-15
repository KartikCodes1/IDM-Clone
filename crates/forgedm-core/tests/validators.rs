//! S1.2 validator tests (THREAT_MODEL NET-02, Inv-05; ARCHITECTURE 4.7).

use forgedm_core::http::validators::{
    EntityTag, EntityTagError, ResumeIdentity, ValidatorCheck, Validators, if_range_value,
    last_modified_is_strong,
};

const LM: &str = "Sun, 06 Nov 1994 08:49:37 GMT";
const DATE_PLUS_1: &str = "Sun, 06 Nov 1994 08:49:38 GMT";
const DATE_SAME: &str = "Sun, 06 Nov 1994 08:49:37 GMT";
const DATE_BEFORE: &str = "Sun, 06 Nov 1994 08:49:36 GMT";

fn v(etag: Option<&str>, lm: Option<&str>, date: Option<&str>) -> Validators {
    Validators {
        etag: etag.map(str::to_owned),
        last_modified: lm.map(str::to_owned),
        date: date.map(str::to_owned),
    }
}

#[test]
fn etag_parse_forms() {
    let strong = EntityTag::parse("\"abc\"").unwrap();
    assert!(!strong.is_weak());
    assert_eq!(strong.opaque(), "abc");
    let weak = EntityTag::parse(" W/\"abc\" ").unwrap();
    assert!(weak.is_weak());
    assert_eq!(weak.opaque(), "abc");
    assert!(EntityTag::parse("\"\"").is_ok());
    for bad in [
        "abc",
        "\"abc",
        "abc\"",
        "w/\"abc\"",
        "W/abc",
        "\"a\"b\"",
        "\"a b\"",
        "",
        "W/",
        "\"a\u{7}b\"",
    ] {
        assert!(EntityTag::parse(bad).is_err(), "{bad:?} must be rejected");
    }
    let too_long = format!("\"{}\"", "a".repeat(2048));
    assert_eq!(EntityTag::parse(&too_long), Err(EntityTagError::TooLong));
}

#[test]
fn strong_compare_requires_both_strong() {
    let s1 = EntityTag::parse("\"x\"").unwrap();
    let s2 = EntityTag::parse("\"x\"").unwrap();
    let s3 = EntityTag::parse("\"y\"").unwrap();
    let w1 = EntityTag::parse("W/\"x\"").unwrap();
    let w2 = EntityTag::parse("W/\"x\"").unwrap();
    assert!(s1.strong_eq(&s2));
    assert!(!s1.strong_eq(&s3));
    assert!(!s1.strong_eq(&w1), "strong vs weak never matches");
    assert!(!w1.strong_eq(&s1));
    assert!(!w1.strong_eq(&w2), "two weak tags never strong-match");
    // Case matters inside the opaque tag.
    let upper = EntityTag::parse("\"X\"").unwrap();
    assert!(!s1.strong_eq(&upper));
}

#[test]
fn if_range_never_weak() {
    // Strong ETag is sent byte-identical.
    assert_eq!(
        if_range_value(&v(Some("\"abc\""), None, None)).as_deref(),
        Some("\"abc\"")
    );
    // A weak ETag is never sent.
    assert_eq!(if_range_value(&v(Some("W/\"abc\""), None, None)), None);
    // A weak ETag blocks the Last-Modified fallback too (only when there is no ETag).
    assert_eq!(
        if_range_value(&v(Some("W/\"abc\""), Some(LM), Some(DATE_PLUS_1))),
        None
    );
    // A strong ETag wins over a strong Last-Modified.
    assert_eq!(
        if_range_value(&v(Some("\"abc\""), Some(LM), Some(DATE_PLUS_1))).as_deref(),
        Some("\"abc\"")
    );
    // An unparseable ETag counts as an ETag that exists: nothing is sent.
    assert_eq!(
        if_range_value(&v(Some("garbage"), Some(LM), Some(DATE_PLUS_1))),
        None
    );
    assert_eq!(if_range_value(&Validators::default()), None);
}

#[test]
fn lm_strength_rule() {
    assert!(last_modified_is_strong(LM, Some(DATE_PLUS_1)));
    assert!(
        !last_modified_is_strong(LM, Some(DATE_SAME)),
        "Date == LM is weak"
    );
    assert!(
        !last_modified_is_strong(LM, Some(DATE_BEFORE)),
        "Date < LM is weak"
    );
    assert!(!last_modified_is_strong(LM, None), "no Date is weak");
    assert!(!last_modified_is_strong("not a date", Some(DATE_PLUS_1)));
    assert!(!last_modified_is_strong(LM, Some("not a date")));
    // Last-Modified is sent byte-identical, only when strong and there is no ETag.
    assert_eq!(
        if_range_value(&v(None, Some(LM), Some(DATE_PLUS_1))).as_deref(),
        Some(LM)
    );
    assert_eq!(if_range_value(&v(None, Some(LM), Some(DATE_SAME))), None);
    assert_eq!(if_range_value(&v(None, Some(LM), None)), None);
    // A non-canonical (but parseable) spelling is sent as received.
    let odd = "Sunday, 06-Nov-94 08:49:37 GMT";
    assert_eq!(
        if_range_value(&v(None, Some(odd), Some(DATE_PLUS_1))).as_deref(),
        Some(odd)
    );
}

#[test]
fn resume_identity_prefers_strong_etag_then_strong_lm() {
    let id =
        ResumeIdentity::from_validators(Some(10), &v(Some("\"e\""), Some(LM), Some(DATE_PLUS_1)));
    assert_eq!(id.total, Some(10));
    assert_eq!(
        id.check(&v(Some("\"e\""), None, None)),
        ValidatorCheck::Same
    );
    assert_eq!(
        id.check(&v(Some("\"f\""), None, None)),
        ValidatorCheck::Different
    );
    assert_eq!(
        id.check(&v(Some("W/\"e\""), None, None)),
        ValidatorCheck::Different
    );
    assert_eq!(
        id.check(&v(None, Some(LM), Some(DATE_PLUS_1))),
        ValidatorCheck::Unknown
    );

    let lm_id = ResumeIdentity::from_validators(Some(10), &v(None, Some(LM), Some(DATE_PLUS_1)));
    assert_eq!(lm_id.check(&v(None, Some(LM), None)), ValidatorCheck::Same);
    assert_eq!(
        lm_id.check(&v(None, Some("Mon, 07 Nov 1994 08:49:37 GMT"), None)),
        ValidatorCheck::Different
    );
    assert_eq!(lm_id.check(&v(None, None, None)), ValidatorCheck::Unknown);
}

#[test]
fn resume_identity_without_strong_validator_has_nothing_to_contradict() {
    for weak in [
        v(Some("W/\"e\""), Some(LM), Some(DATE_PLUS_1)),
        v(None, Some(LM), Some(DATE_SAME)),
        Validators::default(),
    ] {
        let id = ResumeIdentity::from_validators(Some(10), &weak);
        assert!(!id.has_strong_validator());
        assert_eq!(
            id.check(&v(Some("\"anything\""), None, None)),
            ValidatorCheck::Same
        );
    }
    assert!(
        ResumeIdentity::from_validators(None, &v(Some("\"e\""), None, None)).has_strong_validator()
    );
}
