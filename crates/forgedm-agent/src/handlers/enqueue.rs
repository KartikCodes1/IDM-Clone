//! EnqueueUrl and GetDownload. Owned by S1.7; `not_implemented` in this slice.

use forgedm_protocol::{DownloadIdRequest, EnqueueUrlRequest, ResultBody};

use crate::error::AgentError;

/// S1.7 adds URL validation, naming, sealing and the insert.
pub(super) fn enqueue_url(_request: &EnqueueUrlRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}

/// S1.7 adds the detail view (events, segments, validators).
pub(super) fn get_download(_request: &DownloadIdRequest) -> Result<ResultBody, AgentError> {
    Err(AgentError::NotImplemented)
}
