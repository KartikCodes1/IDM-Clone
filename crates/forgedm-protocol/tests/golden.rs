//! Golden files in tests/golden/v1 are the contract shared with the desktop TS tests.
//! Each must parse and re-serialize value-equal.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use forgedm_protocol::{
    Action, ErrorCode, ResultBody, ServerMessage, parse_request, parse_server_message,
};
use serde_json::Value;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const EVENTS: [&str; 9] = [
    "AgentStatus",
    "DownloadAdded",
    "DownloadChanged",
    "ProgressSnapshot",
    "DownloadCompleted",
    "DownloadFailed",
    "DownloadRemoved",
    "QueueChanged",
    "ResyncRequired",
];

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("v1")
}

fn files() -> TestResult<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(golden_dir())? {
        let path = entry?.path();
        assert_eq!(
            path.extension().and_then(|e| e.to_str()),
            Some("json"),
            "{path:?}"
        );
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("golden file name is not UTF-8")?
            .to_owned();
        out.push((stem, fs::read(&path)?));
    }
    out.sort();
    Ok(out)
}

/// `request_EnqueueUrl_minimal` -> ("request", "EnqueueUrl"); error files keep their
/// full snake_case code: `error_not_found` -> ("error", "not_found").
fn split_stem(stem: &str) -> TestResult<(&str, &str)> {
    let (kind, rest) = stem
        .split_once('_')
        .ok_or("golden name has no kind prefix")?;
    if kind == "error" {
        return Ok((kind, rest));
    }
    Ok((kind, rest.split('_').next().ok_or("empty golden name")?))
}

fn check_request(stem: &str, bytes: &[u8], original: &Value) -> TestResult {
    let request = parse_request(bytes).map_err(|e| format!("{stem}: {e}"))?;
    request.validate().map_err(|e| format!("{stem}: {e}"))?;
    assert_eq!(&request.to_value()?, original, "{stem}");
    Ok(())
}

fn check_response(stem: &str, name: &str, bytes: &[u8], original: &Value) -> TestResult {
    let ServerMessage::Response(response) = parse_server_message(bytes)? else {
        return Err(format!("{stem}: not a response").into());
    };
    assert_eq!(&serde_json::to_value(&response)?, original, "{stem}");
    let action: Action = name.parse()?;
    let result = response.result().ok_or("response has no result")?;
    let typed = ResultBody::from_value(action, result.clone())?;
    assert_eq!(typed.action(), action);
    assert_eq!(&typed.to_value()?, &original["result"], "{stem}");
    Ok(())
}

fn check_event(stem: &str, name: &str, bytes: &[u8], original: &Value) -> TestResult {
    let ServerMessage::Event(event) = parse_server_message(bytes)? else {
        return Err(format!("{stem}: not an event").into());
    };
    assert_eq!(event.body.event_name(), name);
    assert_eq!(&serde_json::to_value(&event)?, original, "{stem}");
    Ok(())
}

fn check_error(stem: &str, bytes: &[u8], original: &Value) -> TestResult {
    let ServerMessage::Response(response) = parse_server_message(bytes)? else {
        return Err(format!("{stem}: not a response").into());
    };
    let error = response.error().ok_or("response has no error")?;
    assert_ne!(error.code, ErrorCode::Unknown, "{stem}");
    assert_eq!(&serde_json::to_value(&response)?, original, "{stem}");
    Ok(())
}

#[test]
fn golden_files_roundtrip_value_equal() -> TestResult {
    let mut seen = BTreeSet::new();
    for (stem, bytes) in files()? {
        let original: Value = serde_json::from_slice(&bytes)?;
        let (kind, name) = split_stem(&stem)?;
        match kind {
            "request" => check_request(&stem, &bytes, &original)?,
            "response" => check_response(&stem, name, &bytes, &original)?,
            "event" => check_event(&stem, name, &bytes, &original)?,
            "error" => check_error(&stem, &bytes, &original)?,
            other => return Err(format!("unexpected golden kind {other}").into()),
        }
        seen.insert((kind.to_owned(), name.to_owned()));
    }

    // Completeness: every action has a request and a response, every event a file.
    for action in Action::ALL {
        let name = action.as_str();
        assert!(
            seen.contains(&("request".to_owned(), name.to_owned())),
            "request {name}"
        );
        assert!(
            seen.contains(&("response".to_owned(), name.to_owned())),
            "response {name}"
        );
    }
    for name in EVENTS {
        assert!(
            seen.contains(&("event".to_owned(), name.to_owned())),
            "event {name}"
        );
    }
    for code in [
        "not_found",
        "invalid_state",
        "unsupported_protocol_version",
        "invalid_payload",
        "busy",
        "invalid_url",
        "invalid_destination",
    ] {
        assert!(
            seen.contains(&("error".to_owned(), code.to_owned())),
            "error {code}"
        );
    }
    Ok(())
}
