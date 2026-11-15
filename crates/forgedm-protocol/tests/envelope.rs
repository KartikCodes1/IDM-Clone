//! Two-step strict parsing of requests, and tolerant parsing of server messages.

use forgedm_protocol::{
    ErrorCode, ParseError, Request, RequestBody, ServerMessage, parse_request, parse_server_message,
};
use serde_json::json;

const ID: &str = "018f2c3e-5a10-7b2d-9c4e-0123456789ab";

fn parse(value: serde_json::Value) -> Result<Request, ParseError> {
    parse_request(value.to_string().as_bytes())
}

fn code_of(err: &ParseError) -> ErrorCode {
    err.to_error_body().code
}

#[test]
fn unknown_action_and_unknown_field_rejected() {
    let unknown_action =
        parse(json!({"protocol_version":1,"request_id":7,"action":"Frobnicate","payload":{}}));
    let err = unknown_action.unwrap_err();
    assert_eq!(code_of(&err), ErrorCode::UnknownAction);
    assert_eq!(err.request_id(), Some(7));
    assert!(!err.closes_connection());

    // Lower-case action names are not accepted: casing is part of the contract.
    let err = parse(json!({"protocol_version":1,"request_id":7,"action":"ping","payload":{}}))
        .unwrap_err();
    assert_eq!(code_of(&err), ErrorCode::UnknownAction);

    // Unknown envelope field.
    let err =
        parse(json!({"protocol_version":1,"request_id":1,"action":"Ping","payload":{},"extra":1}))
            .unwrap_err();
    assert_eq!(code_of(&err), ErrorCode::InvalidPayload);
    let details = err.to_error_body().details.unwrap();
    assert_eq!(details.field.as_deref(), Some("extra"));
    assert_eq!(details.reason.as_deref(), Some("unknown_field"));

    // Unknown payload field (also on an empty payload).
    for (action, payload) in [
        ("Ping", json!({"x":1})),
        ("Pause", json!({"download_id":ID,"bogus":true})),
    ] {
        let err =
            parse(json!({"protocol_version":1,"request_id":2,"action":action,"payload":payload}))
                .unwrap_err();
        assert_eq!(code_of(&err), ErrorCode::InvalidPayload, "{action}");
        assert_eq!(err.request_id(), Some(2));
    }

    // Missing and mistyped fields.
    let err =
        parse(json!({"protocol_version":1,"request_id":3,"action":"ListDownloads","payload":{}}))
            .unwrap_err();
    assert_eq!(code_of(&err), ErrorCode::InvalidPayload);
    assert_eq!(
        err.to_error_body().details.unwrap().field.as_deref(),
        Some("limit")
    );
    let err = parse(json!({"protocol_version":1,"request_id":"x","action":"Ping","payload":{}}))
        .unwrap_err();
    assert_eq!(code_of(&err), ErrorCode::InvalidPayload);
    let err = parse(
        json!({"protocol_version":1,"request_id":4_294_967_296u64,"action":"Ping","payload":{}}),
    )
    .unwrap_err();
    assert_eq!(code_of(&err), ErrorCode::InvalidPayload);

    // Duplicate envelope keys are rejected rather than last-one-wins (payload keys: see
    // `duplicate_payload_keys_rejected`).
    let dup =
        br#"{"protocol_version":1,"request_id":1,"request_id":2,"action":"Ping","payload":{}}"#;
    assert_eq!(
        code_of(&parse_request(dup).unwrap_err()),
        ErrorCode::InvalidPayload
    );

    // A good request parses.
    let ok =
        parse(json!({"protocol_version":1,"request_id":9,"action":"Ping","payload":{}})).unwrap();
    assert_eq!(ok.request_id, 9);
    assert!(matches!(ok.body, RequestBody::Ping(_)));
}

#[test]
fn bad_protocol_version_and_malformed_bodies_map_to_codes() {
    let err = parse(json!({"protocol_version":2,"request_id":5,"action":"Ping","payload":{}}))
        .unwrap_err();
    assert_eq!(code_of(&err), ErrorCode::UnsupportedProtocolVersion);
    assert_eq!(err.request_id(), Some(5));
    let details = err.to_error_body().details.unwrap();
    assert_eq!((details.server_min, details.server_max), (Some(1), Some(1)));

    for body in [&b"[]"[..], b"not json", &[0xff]] {
        let err = parse_request(body).unwrap_err();
        assert_eq!(code_of(&err), ErrorCode::MalformedFrame);
        assert!(err.closes_connection());
        assert_eq!(err.request_id(), None);
    }
}

#[test]
fn error_reasons_never_echo_input_values() {
    let secret = "https://user:hunter2@example.com/x";
    let err = parse(json!({
        "protocol_version":1,"request_id":1,"action":"GetDownload",
        "payload":{"download_id":secret}
    }))
    .unwrap_err();
    let text = serde_json::to_string(&err.to_error_body()).unwrap();
    assert!(!text.contains("hunter2"), "{text}");
}

#[test]
fn server_messages_parse_and_unknown_events_are_reported_not_fatal() {
    let event = br#"{"type":"event","seq":3,"event":"DownloadRemoved","data":{"id":"018f2c3e-5a10-7b2d-9c4e-0123456789ab"}}"#;
    assert!(matches!(
        parse_server_message(event),
        Ok(ServerMessage::Event(_))
    ));

    let future = br#"{"type":"event","seq":4,"event":"FromTheFuture","data":{"a":1}}"#;
    assert_eq!(
        parse_server_message(future),
        Ok(ServerMessage::UnknownEvent {
            seq: 4,
            event: "FromTheFuture".to_owned()
        })
    );

    // ok=true needs a result and no error; ok=false needs an error.
    for bad in [
        r#"{"type":"response","request_id":1,"ok":true}"#,
        r#"{"type":"response","request_id":1,"ok":false}"#,
        r#"{"type":"response","request_id":1,"ok":true,"result":{},"error":null}"#,
        r#"{"type":"bogus"}"#,
        r#"[]"#,
    ] {
        assert!(parse_server_message(bad.as_bytes()).is_err(), "{bad}");
    }

    // Unknown error codes are tolerated and honour `retryable`.
    let tolerant = br#"{"type":"response","request_id":1,"ok":false,"error":{"code":"brand_new","message":"m","retryable":true,"details":null}}"#;
    let ServerMessage::Response(response) = parse_server_message(tolerant).unwrap() else {
        unreachable!("the frame is a response");
    };
    let error = response.error().unwrap();
    assert_eq!(error.code, ErrorCode::Unknown);
    assert!(error.retryable);
}

#[test]
fn duplicate_payload_keys_rejected() {
    let nested = br#"{"protocol_version":1,"request_id":1,"action":"EnqueueUrl","payload":{"url":"https://a.example/","url":"https://b.example/"}}"#;
    let err = parse_request(nested).unwrap_err();
    assert_eq!(code_of(&err), ErrorCode::InvalidPayload);
    let details = err.to_error_body().details.unwrap();
    assert_eq!(details.reason.as_deref(), Some("duplicate_field"));
    assert_eq!(details.field.as_deref(), Some("url"));
    assert_eq!(err.request_id(), Some(1));

    // Deeper than the top level of the payload.
    let deep = br#"{"protocol_version":1,"request_id":2,"action":"ListDownloads","payload":{"limit":1,"states":[{"a":1,"a":2}]}}"#;
    assert_eq!(
        code_of(&parse_request(deep).unwrap_err()),
        ErrorCode::InvalidPayload
    );
}

#[test]
fn serde_error_wording_maps_to_stable_reasons() {
    let cases: [(&str, &str, Option<&str>); 5] = [
        (
            r#"{"protocol_version":1,"request_id":1,"action":"Ping","payload":{},"extra":1}"#,
            "unknown_field",
            Some("extra"),
        ),
        (
            r#"{"protocol_version":1,"request_id":1,"action":"ListDownloads","payload":{}}"#,
            "missing_field",
            Some("limit"),
        ),
        (
            r#"{"protocol_version":1,"request_id":1,"request_id":2,"action":"Ping","payload":{}}"#,
            "duplicate_field",
            Some("request_id"),
        ),
        (
            r#"{"protocol_version":1,"request_id":1,"action":"ListDownloads","payload":{"limit":"x"}}"#,
            "invalid_type",
            None,
        ),
        (
            r#"{"protocol_version":1,"request_id":1,"action":"EnqueueUrl","payload":{"url":"https://a.example/","priority":"nope"}}"#,
            "unknown_variant",
            None,
        ),
    ];
    for (body, reason, field) in cases {
        let err = parse_request(body.as_bytes()).unwrap_err();
        let details = err.to_error_body().details.unwrap();
        assert_eq!(details.reason.as_deref(), Some(reason), "{body}");
        assert_eq!(details.field.as_deref(), field, "{body}");
    }
}

#[test]
fn debug_of_enqueue_request_redacts_url_credentials() {
    let body = br#"{"protocol_version":1,"request_id":1,"action":"EnqueueUrl","payload":{"url":"https://user:hunter2@example.com/x?token=abc"}}"#;
    let request = parse_request(body).unwrap();
    let text = format!("{request:?}");
    assert!(
        !text.contains("hunter2") && !text.contains("token=abc"),
        "{text}"
    );
}

#[test]
fn unknown_variant_serializes_as_its_as_str() {
    let value = serde_json::to_value(ErrorCode::Unknown).unwrap();
    assert_eq!(value, json!(ErrorCode::Unknown.as_str()));
    assert_eq!(value, json!("unknown"));
}
