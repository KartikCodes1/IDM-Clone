//! Error bodies carried in failure responses and in `last_error` (Appendix A.1, A.7).
//!
//! `message` is generic text and `details` is typed; neither ever contains secrets or
//! echoes request values.

use serde::{Deserialize, Serialize};

use crate::constants::PROTOCOL_VERSION;
use crate::enums::{
    DownloadErrorCode, DownloadState, ErrorCode, InvalidDestinationReason, InvalidUrlReason,
};

/// Typed details of a response error. Only the fields relevant to the code are set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetails {
    /// `unsupported_protocol_version`: lowest major the agent serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_min: Option<u32>,
    /// `unsupported_protocol_version`: highest major the agent serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_max: Option<u32>,
    /// `invalid_payload`: the offending field, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// `invalid_payload`, `invalid_url`, `invalid_destination`: a short machine reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `invalid_state`: the state the download is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_state: Option<DownloadState>,
}

/// The `error` object of a failure response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Machine-readable code.
    pub code: ErrorCode,
    /// Generic human-readable text.
    pub message: String,
    /// Whether the same request may succeed if retried later.
    pub retryable: bool,
    /// Typed details; serialized as `null` when absent.
    #[serde(default)]
    pub details: Option<ErrorDetails>,
}

impl ErrorBody {
    /// An error with the code's generic message and no details.
    #[must_use]
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            message: code.default_message().to_owned(),
            retryable: code.retryable(),
            details: None,
        }
    }

    /// Replaces the details.
    #[must_use]
    pub fn with_details(self, details: ErrorDetails) -> Self {
        Self {
            details: Some(details),
            ..self
        }
    }

    /// `unsupported_protocol_version` advertising the served range (always `1..=1`).
    #[must_use]
    pub fn unsupported_protocol_version() -> Self {
        Self::new(ErrorCode::UnsupportedProtocolVersion).with_details(ErrorDetails {
            server_min: Some(PROTOCOL_VERSION),
            server_max: Some(PROTOCOL_VERSION),
            ..ErrorDetails::default()
        })
    }

    /// `invalid_payload` naming the field (if known) and a short static reason.
    #[must_use]
    pub fn invalid_payload(field: Option<&str>, reason: &str) -> Self {
        Self::new(ErrorCode::InvalidPayload).with_details(ErrorDetails {
            field: field.map(str::to_owned),
            reason: Some(reason.to_owned()),
            ..ErrorDetails::default()
        })
    }

    /// `invalid_state` carrying the current state.
    #[must_use]
    pub fn invalid_state(current_state: DownloadState) -> Self {
        Self::new(ErrorCode::InvalidState).with_details(ErrorDetails {
            current_state: Some(current_state),
            ..ErrorDetails::default()
        })
    }

    /// `invalid_url` with a typed reason.
    #[must_use]
    pub fn invalid_url(reason: InvalidUrlReason) -> Self {
        Self::new(ErrorCode::InvalidUrl).with_details(ErrorDetails {
            reason: Some(reason.as_str().to_owned()),
            ..ErrorDetails::default()
        })
    }

    /// `invalid_destination` with a typed reason.
    #[must_use]
    pub fn invalid_destination(reason: InvalidDestinationReason) -> Self {
        Self::new(ErrorCode::InvalidDestination).with_details(ErrorDetails {
            reason: Some(reason.as_str().to_owned()),
            ..ErrorDetails::default()
        })
    }
}

/// Parameters of a download failure code (`rate_limited`, `http_status`, ...).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadErrorDetails {
    /// `rate_limited`: epoch milliseconds when the server allows a retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<u64>,
    /// `http_status`: the status code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// `range_protocol_violation`: a short machine reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `retry_budget_exhausted`: the last failure that was retried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<DownloadErrorCode>,
}

/// A download failure: `last_error` and the `DownloadFailed` event's `error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadError {
    /// Machine-readable code.
    pub code: DownloadErrorCode,
    /// Generic, redacted text.
    pub message: String,
    /// Whether an automatic or user retry can succeed.
    pub retryable: bool,
    /// Code parameters (additive within v1; absent for codes without parameters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<DownloadErrorDetails>,
}
