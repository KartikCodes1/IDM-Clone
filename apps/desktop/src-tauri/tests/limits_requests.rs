//! Request construction for the batch, limits and priority commands (PLAN A.3): exact wire
//! payloads, the three-state rate limit, and local range checks before any pipe call.

#![allow(clippy::expect_used)] // test code may expect (ARCHITECTURE 1.4)

use forgedm_desktop_lib::error::CommandErrorKind;
use forgedm_desktop_lib::request::{self, LimitsChange, RateLimitChoice};
use forgedm_protocol::RequestBody;
use serde_json::{Value, json};

const ID: &str = "018f2c3e-5a10-7b2d-9c4e-0123456789ab";

fn payload(body: &RequestBody) -> Value {
    body.payload_value().expect("payload serializes")
}

fn change(
    max_connections: Option<u32>,
    rate_limit: Option<RateLimitChoice>,
    max_active_downloads: Option<u32>,
) -> LimitsChange {
    LimitsChange {
        max_connections,
        rate_limit,
        max_active_downloads,
    }
}

#[test]
fn pause_all_defaults_to_keeping_progress_and_can_discard() {
    let keep = request::pause_all(false);
    assert!(matches!(keep, RequestBody::PauseAll(_)));
    assert_eq!(payload(&keep), json!({"discard_progress": false}));
    assert_eq!(
        payload(&request::pause_all(true)),
        json!({"discard_progress": true})
    );
}

#[test]
fn resume_all_sends_an_empty_payload() {
    let body = request::resume_all();
    assert!(matches!(body, RequestBody::ResumeAll(_)));
    assert_eq!(payload(&body), json!({}));
}

#[test]
fn get_limits_is_a_global_update_that_changes_nothing() {
    let body = request::get_limits();
    assert!(matches!(body, RequestBody::UpdateLimits(_)));
    assert_eq!(payload(&body), json!({"scope": "global"}));
}

#[test]
fn global_update_carries_only_the_given_fields() {
    let body =
        request::update_limits("global", None, &change(Some(8), None, Some(4))).expect("valid");
    assert_eq!(
        payload(&body),
        json!({"scope": "global", "max_connections": 8, "max_active_downloads": 4})
    );
}

#[test]
fn off_sends_null_to_clear_the_cap_and_a_number_sets_it() {
    let off = request::update_limits(
        "global",
        None,
        &change(None, Some(RateLimitChoice::Off), None),
    )
    .expect("valid");
    assert_eq!(
        payload(&off),
        json!({"scope": "global", "rate_limit_bps": null})
    );
    let set = request::update_limits(
        "global",
        None,
        &change(None, Some(RateLimitChoice::BytesPerSecond(2048)), None),
    )
    .expect("valid");
    assert_eq!(
        payload(&set),
        json!({"scope": "global", "rate_limit_bps": 2048})
    );
}

#[test]
fn download_scope_names_the_download_and_validates_the_id() {
    let body =
        request::update_limits("download", Some(ID), &change(Some(3), None, None)).expect("valid");
    assert_eq!(
        payload(&body),
        json!({"scope": {"download_id": ID}, "max_connections": 3})
    );
    for bad in [None, Some("nope"), Some("")] {
        let err = request::update_limits("download", bad, &change(Some(3), None, None))
            .expect_err("bad id");
        assert_eq!(err.kind, CommandErrorKind::InvalidRequest);
    }
}

#[test]
fn out_of_range_values_are_refused_before_any_call() {
    let cases = [
        change(Some(0), None, None),
        change(Some(17), None, None),
        change(None, None, Some(0)),
        change(None, None, Some(11)),
        change(None, Some(RateLimitChoice::BytesPerSecond(1023)), None),
        change(None, Some(RateLimitChoice::BytesPerSecond(0)), None),
    ];
    for case in &cases {
        let err = request::update_limits("global", None, case).expect_err("out of range");
        assert_eq!(err.kind, CommandErrorKind::InvalidRequest, "{case:?}");
    }
    let edge = change(
        Some(16),
        Some(RateLimitChoice::BytesPerSecond(1024)),
        Some(10),
    );
    assert!(request::update_limits("global", None, &edge).is_ok());
}

#[test]
fn max_active_downloads_is_global_only_and_scope_is_a_closed_set() {
    let err = request::update_limits("download", Some(ID), &change(None, None, Some(2)))
        .expect_err("global only");
    assert_eq!(err.kind, CommandErrorKind::InvalidRequest);
    let err = request::update_limits("everything", None, &change(Some(2), None, None))
        .expect_err("unknown scope");
    assert_eq!(err.kind, CommandErrorKind::InvalidRequest);
}

#[test]
fn rate_limit_choice_reads_off_or_a_number_from_the_webview() {
    let off: RateLimitChoice = serde_json::from_value(json!("off")).expect("off");
    assert_eq!(off, RateLimitChoice::Off);
    let num: RateLimitChoice = serde_json::from_value(json!(4096)).expect("number");
    assert_eq!(num, RateLimitChoice::BytesPerSecond(4096));
    for bad in [
        json!("fast"),
        json!(-5),
        json!(1.5),
        json!(null),
        json!(true),
    ] {
        assert!(
            serde_json::from_value::<RateLimitChoice>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn priority_maps_the_three_words_and_rejects_anything_else() {
    for word in ["high", "normal", "low"] {
        let body = request::update_priority(ID, word).expect("valid");
        assert!(matches!(body, RequestBody::UpdatePriority(_)));
        assert_eq!(payload(&body), json!({"download_id": ID, "priority": word}));
    }
    for bad in ["", "urgent", "HIGH", "unknown"] {
        let err = request::update_priority(ID, bad).expect_err("bad priority");
        assert_eq!(err.kind, CommandErrorKind::InvalidRequest, "{bad}");
    }
    let err = request::update_priority("nope", "high").expect_err("bad id");
    assert_eq!(err.kind, CommandErrorKind::InvalidRequest);
}
