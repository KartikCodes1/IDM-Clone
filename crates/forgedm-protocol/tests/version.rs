//! Version negotiation and stable wire strings.

use forgedm_protocol::{Action, DownloadErrorCode, DownloadState, ErrorCode, negotiate_version};

#[test]
fn version_negotiation_accepts_1_rejects_0_and_2() {
    assert_eq!(negotiate_version(1, 1), Ok(1));
    assert_eq!(negotiate_version(0, 1), Ok(1));
    assert_eq!(negotiate_version(1, 5), Ok(1));
    for (min, max) in [(0, 0), (2, 2), (2, 9), (3, 1)] {
        let err = negotiate_version(min, max).unwrap_err();
        let body = err.to_error_body();
        assert_eq!(body.code, ErrorCode::UnsupportedProtocolVersion);
        assert!(!body.retryable);
        let details = body.details.unwrap();
        assert_eq!((details.server_min, details.server_max), (Some(1), Some(1)));
    }
}

#[test]
fn state_and_error_code_strings_stable() {
    let states = [
        "queued",
        "probing",
        "downloading",
        "pausing",
        "paused",
        "retry_waiting",
        "verifying",
        "finalizing",
        "completed",
        "failed",
        "cancelled",
        "interrupted",
        "needs_credentials",
        "needs_url_refresh",
    ];
    assert_eq!(DownloadState::ALL.len(), 14);
    for (state, expected) in DownloadState::ALL.iter().zip(states) {
        assert_eq!(state.as_str(), expected);
        assert_eq!(serde_json::to_value(state).unwrap(), expected);
        let back: DownloadState = serde_json::from_value(expected.into()).unwrap();
        assert_eq!(back, *state);
    }
    assert_eq!(
        serde_json::from_value::<DownloadState>("from_the_future".into()).unwrap(),
        DownloadState::Unknown
    );

    let codes = [
        "handshake_required",
        "malformed_frame",
        "frame_too_large",
        "unsupported_protocol_version",
        "unknown_action",
        "invalid_payload",
        "not_found",
        "invalid_state",
        "not_resumable",
        "invalid_url",
        "invalid_destination",
        "invalid_file_name",
        "invalid_checksum",
        "limit_exceeded",
        "not_implemented",
        "internal",
        "busy",
        "shutting_down",
        "resync_required",
        "storage_unavailable",
    ];
    assert_eq!(ErrorCode::ALL.len(), codes.len());
    for (code, expected) in ErrorCode::ALL.iter().zip(codes) {
        assert_eq!(code.as_str(), expected);
        assert_eq!(serde_json::to_value(code).unwrap(), expected);
    }
    let retryable: Vec<&str> = ErrorCode::ALL
        .iter()
        .filter(|c| c.retryable())
        .map(|c| c.as_str())
        .collect();
    assert_eq!(
        retryable,
        [
            "busy",
            "shutting_down",
            "resync_required",
            "storage_unavailable"
        ]
    );
    assert_eq!(
        serde_json::from_value::<ErrorCode>("new_code".into()).unwrap(),
        ErrorCode::Unknown
    );

    let download_codes = [
        "network_timeout",
        "network_connect",
        "network_reset",
        "body_incomplete",
        "dns_failure",
        "rate_limited",
        "http_status",
        "auth_required",
        "url_expired",
        "tls_failure",
        "too_many_redirects",
        "redirect_loop",
        "redirect_downgrade",
        "redirect_blocked",
        "range_protocol_violation",
        "unsupported_content_encoding",
        "remote_changed",
        "size_mismatch",
        "checksum_mismatch",
        "disk_full",
        "destination_unavailable",
        "disk_io",
        "path_too_long",
        "file_system_limit",
        "secrets_unavailable",
        "blocked_by_policy",
        "blocked_by_antivirus",
        "internal",
        "path_escape",
        "finalize_locked",
        "retry_budget_exhausted",
        "part_file_invalid",
    ];
    assert_eq!(DownloadErrorCode::ALL.len(), download_codes.len());
    for (code, expected) in DownloadErrorCode::ALL.iter().zip(download_codes) {
        assert_eq!(code.as_str(), expected);
        assert_eq!(serde_json::to_value(code).unwrap(), expected);
    }

    let actions = [
        "Handshake",
        "Ping",
        "GetAgentStatus",
        "GetSnapshot",
        "Subscribe",
        "Unsubscribe",
        "ListDownloads",
        "ShutdownAgentSafely",
        "EnqueueUrl",
        "GetDownload",
        "Pause",
        "Resume",
        "Cancel",
        "Retry",
        "Remove",
        "PauseAll",
        "ResumeAll",
        "UpdateLimits",
        "UpdatePriority",
    ];
    assert_eq!(Action::ALL.len(), actions.len());
    for (action, expected) in Action::ALL.iter().zip(actions) {
        assert_eq!(action.as_str(), expected);
        assert_eq!(expected.parse::<Action>(), Ok(*action));
    }
}
