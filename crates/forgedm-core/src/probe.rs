//! The probe (ARCHITECTURE 4.4 and 4.5, ADR-009): an optional `HEAD` with a 5 s deadline,
//! then a mandatory `GET Range: bytes=0-0`.
//!
//! The GET decides; the HEAD only supplies a hint. A HEAD that fails, times out or is
//! refused (405) is tolerated. The probe never reads a body: the GET's response is dropped
//! as soon as its headers are read, so a server that ignores the range cannot make the probe
//! download the file.

use std::time::Duration;

use forgedm_protocol::DownloadErrorCode;
use reqwest::header::CONTENT_DISPOSITION;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::client::{ClientError, FetchSpec, HttpClient, response_meta};
use crate::engine::StopReason;
use crate::error::{Failure, TaskStop, failure_from_transfer};
use crate::http::content_range::RangeRequest;
use crate::http::probe_classify::{ProbeClassification, ProbeResult, classify_probe};
use crate::http::range_decision::ResponseMeta;

/// Deadline of the optional `HEAD` (A.9 `HEAD_TIMEOUT`).
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(5);

/// A successful probe: what the GET showed and where it ended up.
#[derive(Debug, Clone)]
pub struct Probed {
    /// The classification: mode, total, resume support and the GET's validators.
    pub result: ProbeResult,
    /// The URL after redirects.
    pub final_url: Url,
    /// The raw `Content-Disposition` of the GET, if any.
    pub content_disposition: Option<String>,
    /// The `Content-Type` of the GET, if any.
    pub mime: Option<String>,
}

fn client_stop(err: ClientError) -> TaskStop {
    match err {
        ClientError::Cancelled => TaskStop::Stopped(StopReason::Cancel),
        ClientError::Redirect(redirect) => TaskStop::Fail(Failure::new(redirect.code())),
        ClientError::Transport(kind) => failure_from_transfer(kind),
        ClientError::Build(_) => TaskStop::Fail(Failure::new(DownloadErrorCode::Internal)),
    }
}

/// The HEAD response, or `None` when it could not be had. Only cancellation is an error.
async fn head(
    client: &HttpClient,
    url: &Url,
    cancel: &CancellationToken,
) -> Result<Option<ResponseMeta>, TaskStop> {
    // The deadline covers the whole HEAD, redirects included.
    match timeout(HEAD_TIMEOUT, client.fetch(url, &FetchSpec::head(), cancel)).await {
        Ok(Ok(fetched)) => Ok(Some(response_meta(&fetched.response))),
        Ok(Err(ClientError::Cancelled)) => Err(TaskStop::Stopped(StopReason::Cancel)),
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "HEAD failed; the probe goes on with the GET");
            Ok(None)
        }
        Err(_elapsed) => {
            tracing::debug!("HEAD timed out; the probe goes on with the GET");
            Ok(None)
        }
    }
}

/// Probes `url`: HEAD (tolerated to fail), then `GET Range: bytes=0-0`, then classifies.
///
/// # Errors
/// [`TaskStop::Stopped`] when `cancel` fires (the caller substitutes the real reason),
/// [`TaskStop::NeedsCredentials`] for 401 or 407, `unsupported_content_encoding`,
/// `range_protocol_violation{reason}`, `http_status{status}` or a transport failure.
pub async fn probe(
    client: &HttpClient,
    url: &Url,
    cancel: &CancellationToken,
) -> Result<Probed, TaskStop> {
    let head = head(client, url, cancel).await?;
    let first_byte = RangeRequest {
        start: 0,
        end_excl: 1,
    };
    let fetched = client
        .fetch(url, &FetchSpec::ranged(first_byte, None), cancel)
        .await
        .map_err(client_stop)?;
    let meta = response_meta(&fetched.response);
    let content_disposition = fetched
        .response
        .headers()
        .get(CONTENT_DISPOSITION)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
    let final_url = fetched.final_url;
    // Dropping the response closes the connection: the body is never read.
    drop(fetched.response);
    match classify_probe(head.as_ref(), &meta) {
        ProbeClassification::Ready(result) => Ok(Probed {
            result,
            final_url,
            content_disposition,
            mime: meta.content_type,
        }),
        ProbeClassification::UnsupportedContentEncoding => {
            Err(Failure::new(DownloadErrorCode::UnsupportedContentEncoding).into())
        }
        ProbeClassification::NeedsCredentials => Err(TaskStop::NeedsCredentials),
        ProbeClassification::UrlRejected { status }
        | ProbeClassification::OtherStatus { status } => Err(Failure::http_status(status).into()),
        ProbeClassification::Violation(violation) => {
            Err(Failure::protocol_violation(violation.reason()).into())
        }
    }
}
