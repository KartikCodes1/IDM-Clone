//! Request construction for the write commands: exact wire payloads, id validation, and
//! the rule that nothing is sent when the webview supplies a malformed id.

#![allow(clippy::expect_used)] // test code may expect (ARCHITECTURE 1.4)

use forgedm_desktop_lib::error::CommandErrorKind;
use forgedm_desktop_lib::request;
use forgedm_protocol::RequestBody;
use serde_json::{Value, json};

const ID: &str = "018f2c3e-5a10-7b2d-9c4e-0123456789ab";

fn payload(body: &RequestBody) -> Value {
    body.payload_value().expect("payload serializes")
}

#[test]
fn enqueue_sends_trimmed_url_and_only_the_given_options() {
    let minimal = request::enqueue_url("  https://example.com/a.zip \n", None, None);
    assert_eq!(
        payload(&minimal),
        json!({"url": "https://example.com/a.zip", "start_paused": false})
    );

    let full = request::enqueue_url(
        "https://example.com/a.zip",
        Some(r"C:\Users\me\Downloads"),
        Some(&"AB".repeat(32)),
    );
    assert_eq!(
        payload(&full),
        json!({
            "url": "https://example.com/a.zip",
            "target_dir": r"C:\Users\me\Downloads",
            "expected_sha256": "AB".repeat(32),
            "start_paused": false,
        })
    );
}

#[test]
fn enqueue_treats_blank_optional_fields_as_absent() {
    let body = request::enqueue_url("https://example.com/a", Some("   "), Some(""));
    assert_eq!(
        payload(&body),
        json!({"url": "https://example.com/a", "start_paused": false})
    );
}

#[test]
fn pause_defaults_to_keeping_progress_and_can_discard() {
    let keep = request::pause(ID, false).expect("valid id");
    assert!(matches!(keep, RequestBody::Pause(_)));
    assert_eq!(
        payload(&keep),
        json!({"download_id": ID, "discard_progress": false})
    );
    let discard = request::pause(ID, true).expect("valid id");
    assert_eq!(
        payload(&discard),
        json!({"download_id": ID, "discard_progress": true})
    );
}

#[test]
fn id_only_actions_map_to_their_action() {
    assert!(matches!(
        request::resume(ID).expect("ok"),
        RequestBody::Resume(_)
    ));
    assert!(matches!(
        request::cancel(ID).expect("ok"),
        RequestBody::Cancel(_)
    ));
    assert!(matches!(
        request::get_download(ID).expect("ok"),
        RequestBody::GetDownload(_)
    ));
    assert_eq!(
        payload(&request::cancel(ID).expect("ok")),
        json!({"download_id": ID})
    );
}

#[test]
fn retry_and_remove_carry_their_flags() {
    let retry = request::retry(ID, true).expect("ok");
    assert_eq!(
        payload(&retry),
        json!({"download_id": ID, "restart_from_zero": true})
    );
    let keep = request::remove(ID, false).expect("ok");
    assert_eq!(
        payload(&keep),
        json!({"download_id": ID, "delete_partial_file": false})
    );
    let delete = request::remove(ID, true).expect("ok");
    assert_eq!(
        payload(&delete),
        json!({"download_id": ID, "delete_partial_file": true})
    );
}

#[test]
fn malformed_ids_are_rejected_as_invalid_request() {
    let long = "x".repeat(5000);
    for bad in [
        "",
        "not-a-uuid",
        "../../etc",
        "018f2c3e-5a10",
        long.as_str(),
    ] {
        let err = request::resume(bad).expect_err("malformed id");
        assert_eq!(err.kind, CommandErrorKind::InvalidRequest, "{bad:?}");
        assert!(
            bad.is_empty() || !err.message.contains(bad),
            "the id is not echoed"
        );
    }
}
