//! `validate()`: the Appendix A.3 limits, enforced on every request before it reaches the
//! engine. Pure and allocation-light; never inspects the network or the file system.

use crate::actions::RequestBody;
use crate::constants::{
    LIST_PAGE_MAX, MAX_ACTIVE_DOWNLOADS, MAX_CLIENT_NAME_CHARS, MAX_CLIENT_VERSION_CHARS,
    MAX_CONNECTIONS, MAX_FILE_NAME_UTF16, MAX_SAFE_INTEGER, MAX_URL_BYTES, MIN_ACTIVE_DOWNLOADS,
    MIN_CONNECTIONS, MIN_RATE_LIMIT_BPS, SHA256_HEX_LEN,
};
use crate::enums::{DownloadState, InvalidDestinationReason, InvalidUrlReason};
use crate::envelope::Request;
use crate::error::ErrorBody;
use crate::payloads::{
    EnqueueUrlRequest, HandshakeRequest, LimitScope, ListDownloadsRequest, UpdateLimitsRequest,
};

/// A request that parsed but violates a documented limit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// `url` is empty.
    #[error("url is empty")]
    UrlEmpty,
    /// `url` exceeds 8192 bytes.
    #[error("url exceeds {MAX_URL_BYTES} bytes")]
    UrlTooLong,
    /// `file_name` is empty.
    #[error("file name is empty")]
    FileNameEmpty,
    /// `file_name` exceeds 255 UTF-16 units.
    #[error("file name exceeds {MAX_FILE_NAME_UTF16} UTF-16 units")]
    FileNameTooLong,
    /// `expected_sha256` is not 64 hex characters.
    #[error("expected_sha256 must be {SHA256_HEX_LEN} hex characters")]
    ChecksumInvalid,
    /// `target_dir` is not an absolute path.
    #[error("target_dir must be absolute")]
    TargetDirNotAbsolute,
    /// A numeric or enumerated field is outside its allowed range.
    #[error("field {field} is out of range")]
    OutOfRange {
        /// The wire field name.
        field: &'static str,
    },
    /// A string field is longer than allowed.
    #[error("field {field} is too long")]
    TooLong {
        /// The wire field name.
        field: &'static str,
    },
}

impl ValidationError {
    /// The response error the agent sends for this violation.
    #[must_use]
    pub fn to_error_body(&self) -> ErrorBody {
        use crate::enums::ErrorCode;
        match self {
            Self::UrlEmpty => ErrorBody::invalid_url(InvalidUrlReason::Parse),
            Self::UrlTooLong => ErrorBody::invalid_url(InvalidUrlReason::TooLong),
            Self::FileNameEmpty | Self::FileNameTooLong => {
                ErrorBody::new(ErrorCode::InvalidFileName)
            }
            Self::ChecksumInvalid => ErrorBody::new(ErrorCode::InvalidChecksum),
            Self::TargetDirNotAbsolute => {
                ErrorBody::invalid_destination(InvalidDestinationReason::NotAbsolute)
            }
            Self::OutOfRange { field } => ErrorBody::invalid_payload(Some(field), "out_of_range"),
            Self::TooLong { field } => ErrorBody::invalid_payload(Some(field), "too_long"),
        }
    }
}

impl Request {
    /// Checks the request against the Appendix A.3 limits.
    ///
    /// # Errors
    /// The first [`ValidationError`] found.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match &self.body {
            RequestBody::Handshake(p) => validate_handshake(p),
            RequestBody::Subscribe(p) => check_js_safe("after_seq", p.after_seq),
            RequestBody::ListDownloads(p) => validate_list(p),
            RequestBody::EnqueueUrl(p) => validate_enqueue(p),
            RequestBody::UpdateLimits(p) => validate_limits(p),
            RequestBody::Ping(_)
            | RequestBody::GetAgentStatus(_)
            | RequestBody::GetSnapshot(_)
            | RequestBody::Unsubscribe(_)
            | RequestBody::ShutdownAgentSafely(_)
            | RequestBody::GetDownload(_)
            | RequestBody::Pause(_)
            | RequestBody::Resume(_)
            | RequestBody::Cancel(_)
            | RequestBody::Retry(_)
            | RequestBody::Remove(_)
            | RequestBody::PauseAll(_)
            | RequestBody::ResumeAll(_)
            | RequestBody::UpdatePriority(_) => Ok(()),
        }
    }
}

fn check_js_safe(field: &'static str, value: u64) -> Result<(), ValidationError> {
    if value > MAX_SAFE_INTEGER {
        Err(ValidationError::OutOfRange { field })
    } else {
        Ok(())
    }
}

fn check_range(field: &'static str, value: u32, min: u32, max: u32) -> Result<(), ValidationError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ValidationError::OutOfRange { field })
    }
}

fn check_rate_limit(rate: Option<Option<u64>>) -> Result<(), ValidationError> {
    let Some(Some(bps)) = rate else { return Ok(()) };
    check_js_safe("rate_limit_bps", bps)?;
    if bps < MIN_RATE_LIMIT_BPS {
        return Err(ValidationError::OutOfRange {
            field: "rate_limit_bps",
        });
    }
    Ok(())
}

fn validate_handshake(p: &HandshakeRequest) -> Result<(), ValidationError> {
    if p.client_name.chars().count() > MAX_CLIENT_NAME_CHARS {
        return Err(ValidationError::TooLong {
            field: "client_name",
        });
    }
    if p.client_version.chars().count() > MAX_CLIENT_VERSION_CHARS {
        return Err(ValidationError::TooLong {
            field: "client_version",
        });
    }
    Ok(())
}

fn validate_list(p: &ListDownloadsRequest) -> Result<(), ValidationError> {
    check_range("limit", p.limit, 1, LIST_PAGE_MAX)?;
    if p.states
        .iter()
        .flatten()
        .any(|s| *s == DownloadState::Unknown)
    {
        return Err(ValidationError::OutOfRange { field: "states" });
    }
    Ok(())
}

fn validate_enqueue(p: &EnqueueUrlRequest) -> Result<(), ValidationError> {
    if p.url.is_empty() {
        return Err(ValidationError::UrlEmpty);
    }
    if p.url.len() > MAX_URL_BYTES {
        return Err(ValidationError::UrlTooLong);
    }
    if let Some(name) = &p.file_name {
        validate_file_name(name)?;
    }
    if let Some(sha) = &p.expected_sha256 {
        validate_sha256(sha)?;
    }
    if let Some(dir) = &p.target_dir
        && !is_absolute_path(dir)
    {
        return Err(ValidationError::TargetDirNotAbsolute);
    }
    if let Some(conns) = p.max_connections {
        check_range("max_connections", conns, MIN_CONNECTIONS, MAX_CONNECTIONS)?;
    }
    check_rate_limit(p.rate_limit_bps)
}

fn validate_file_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() {
        return Err(ValidationError::FileNameEmpty);
    }
    if name.encode_utf16().count() > MAX_FILE_NAME_UTF16 {
        return Err(ValidationError::FileNameTooLong);
    }
    Ok(())
}

fn validate_sha256(sha: &str) -> Result<(), ValidationError> {
    if sha.len() == SHA256_HEX_LEN && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ValidationError::ChecksumInvalid)
    }
}

/// Windows absolute path: `X:\` or `X:/` drive paths, or a `\\` prefix (UNC and `\\?\`
/// device paths; the agent decides later whether those are supported). Rooted-only
/// (`\dir`) and drive-relative (`C:dir`) paths are not absolute.
fn is_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    drive || path.starts_with("\\\\")
}

fn validate_limits(p: &UpdateLimitsRequest) -> Result<(), ValidationError> {
    if let Some(conns) = p.max_connections {
        check_range("max_connections", conns, MIN_CONNECTIONS, MAX_CONNECTIONS)?;
    }
    check_rate_limit(p.rate_limit_bps)?;
    if let Some(active) = p.max_active_downloads {
        if !matches!(p.scope, LimitScope::Global) {
            return Err(ValidationError::OutOfRange {
                field: "max_active_downloads",
            });
        }
        check_range(
            "max_active_downloads",
            active,
            MIN_ACTIVE_DOWNLOADS,
            MAX_ACTIVE_DOWNLOADS,
        )?;
    }
    Ok(())
}
