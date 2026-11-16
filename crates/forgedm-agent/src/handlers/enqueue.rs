//! EnqueueUrl (A.3): validate every client-supplied value, then hand a [`EnqueueSpec`] to the
//! engine. Nothing reaches the engine, the database or the file system unvalidated, and no
//! error echoes the URL or a path.

use forgedm_core::url_policy::validate_user_url;
use forgedm_ipc::Connection;
use forgedm_protocol::{ClientKind, EnqueueUrlRequest, Priority, ResultBody};

use super::AgentHandler;
use crate::destination::{validate_checksum, validate_destination, validate_file_name};
use crate::engine_host::EnqueueSpec;
use crate::error::AgentError;

/// Validates `request` and creates the download.
pub(super) async fn enqueue_url(
    handler: &AgentHandler,
    conn: &Connection,
    request: EnqueueUrlRequest,
) -> Result<ResultBody, AgentError> {
    let engine = handler.engine()?;
    let source = conn
        .hello()
        .map_or(ClientKind::Cli, |hello| hello.client_kind);
    let spec = build_spec(&request, source, engine.default_connections())?;
    Ok(ResultBody::EnqueueUrl(engine.enqueue(spec).await?))
}

/// The validated form of `request`.
fn build_spec(
    request: &EnqueueUrlRequest,
    source: ClientKind,
    default_connections: u32,
) -> Result<EnqueueSpec, AgentError> {
    // Fields that belong to later slices are refused, never silently ignored.
    if matches!(request.rate_limit_bps, Some(Some(_))) {
        return Err(AgentError::Unsupported("rate_limit_bps"));
    }
    if request.start_paused {
        return Err(AgentError::Unsupported("start_paused"));
    }
    let url =
        validate_user_url(&request.url).map_err(|err| AgentError::InvalidUrl(err.reason()))?;
    Ok(EnqueueSpec {
        url,
        target_dir: validate_destination(request.target_dir.as_deref())?,
        file_name: validate_file_name(request.file_name.as_deref())?,
        expected_sha256: validate_checksum(request.expected_sha256.as_deref())?,
        max_connections: request.max_connections.unwrap_or(default_connections),
        priority: request.priority.unwrap_or(Priority::Normal),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_slice_fields_are_refused_not_ignored() {
        let mut limited = EnqueueUrlRequest::new("https://example.com/f.bin");
        limited.rate_limit_bps = Some(Some(2048));
        assert!(matches!(
            build_spec(&limited, ClientKind::Cli, 8),
            Err(AgentError::Unsupported("rate_limit_bps"))
        ));
        let mut paused = EnqueueUrlRequest::new("https://example.com/f.bin");
        paused.start_paused = true;
        assert!(matches!(
            build_spec(&paused, ClientKind::Cli, 8),
            Err(AgentError::Unsupported("start_paused"))
        ));
    }

    #[test]
    fn the_stored_default_connections_fill_in_when_the_request_names_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = EnqueueUrlRequest::new("https://example.com/f.bin");
        request.target_dir = dir.path().to_str().map(str::to_owned);
        assert_eq!(
            build_spec(&request, ClientKind::Cli, 3)
                .unwrap()
                .max_connections,
            3
        );
        request.max_connections = Some(5);
        assert_eq!(
            build_spec(&request, ClientKind::Cli, 3)
                .unwrap()
                .max_connections,
            5,
            "an explicit cap wins"
        );
    }

    #[test]
    fn defaults_fill_in_connections_and_priority() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = EnqueueUrlRequest::new("https://example.com/f.bin#frag");
        request.target_dir = dir.path().to_str().map(str::to_owned);
        let spec = build_spec(&request, ClientKind::Desktop, 8).unwrap();
        assert_eq!(spec.max_connections, 8);
        assert_eq!(spec.priority, Priority::Normal);
        assert_eq!(spec.source, ClientKind::Desktop);
        assert!(!spec.url.as_url().as_str().contains('#'));
    }
}
