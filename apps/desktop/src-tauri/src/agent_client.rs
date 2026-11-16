//! Per-call agent client (Phase 0: connect, handshake, one request, drop).
//!
//! The expected agent image is the canonical `forgedm-agent.exe` next to this executable
//! (PHASE_0_1_PLAN C26); [`forgedm_ipc::Client::connect`] verifies the server's SID and
//! image path before a single byte is sent.

use std::path::{Path, PathBuf};

use forgedm_ipc::{Client, ClientConfig, IpcError};
use forgedm_protocol::{
    AgentStatusInfo, ClientKind, DownloadDetail, Empty, EnqueueUrlResult, ErrorCode,
    ListDownloadsRequest, ListDownloadsResult, PauseAllResult, RequestBody, ResultBody,
    ResumeAllResult, SnapshotResult, StateResult, UpdateLimitsResult,
};

use crate::error::{CommandError, CommandErrorKind};

/// File name of the agent executable shipped next to the desktop executable.
pub const AGENT_EXE_NAME: &str = "forgedm-agent.exe";
/// Page size used for the Phase 0 list (`ListDownloads` accepts `1..=500`).
pub const LIST_PAGE_LIMIT: u32 = 500;
const CLIENT_NAME: &str = "forgedm-desktop";

/// Where the agent listens and which executable it must be running from.
#[derive(Debug, Clone)]
pub struct AgentLocator {
    pipe_name: String,
    expected_agent_path: PathBuf,
}

impl AgentLocator {
    /// A locator for an explicit pipe and agent image (tests, future per-data-root use).
    #[must_use]
    pub fn new(pipe_name: impl Into<String>, expected_agent_path: impl Into<PathBuf>) -> Self {
        Self {
            pipe_name: pipe_name.into(),
            expected_agent_path: expected_agent_path.into(),
        }
    }

    /// The agent image the server must be running from.
    #[must_use]
    pub fn expected_agent_path(&self) -> &Path {
        &self.expected_agent_path
    }

    /// The production locator: the current user's default pipe and the canonical
    /// `forgedm-agent.exe` beside `current_exe()`.
    ///
    /// # Errors
    /// [`CommandErrorKind::AgentUnavailable`] when the agent executable is missing,
    /// [`CommandErrorKind::Internal`] when the user's pipe name cannot be determined.
    pub fn for_current_user() -> Result<Self, CommandError> {
        let exe = std::env::current_exe().map_err(local_fault)?;
        let agent = sibling_agent_path(&exe)?;
        Ok(Self::new(default_pipe_name()?, agent))
    }
}

/// Canonical path of the agent executable next to `exe`.
fn sibling_agent_path(exe: &Path) -> Result<PathBuf, CommandError> {
    let dir = exe.parent().ok_or_else(|| {
        CommandError::new(
            CommandErrorKind::Internal,
            "The application folder is unknown.",
        )
    })?;
    std::fs::canonicalize(dir.join(AGENT_EXE_NAME)).map_err(|err| {
        tracing::warn!(error = %err, "agent executable not found next to the desktop executable");
        CommandError::new(
            CommandErrorKind::AgentUnavailable,
            "The background engine is not installed next to the app.",
        )
    })
}

fn local_fault(err: std::io::Error) -> CommandError {
    tracing::warn!(error = %err, "local fault");
    CommandError::new(CommandErrorKind::Internal, "A local error occurred.")
}

/// The current user's default pipe name, from the shared `forgedm-ipc` helper.
fn default_pipe_name() -> Result<String, CommandError> {
    forgedm_ipc::pipe_name_for(None).map_err(|err| {
        tracing::warn!(error = %err, "the agent pipe name could not be determined");
        CommandError::new(
            CommandErrorKind::Internal,
            "The current user could not be determined.",
        )
    })
}

pub(crate) async fn connect(locator: &AgentLocator) -> Result<Client, CommandError> {
    let config = ClientConfig::new(&locator.pipe_name, &locator.expected_agent_path);
    let mut client = Client::connect(config).await?;
    client
        .handshake(ClientKind::Desktop, CLIENT_NAME, env!("CARGO_PKG_VERSION"))
        .await?;
    Ok(client)
}

pub(crate) fn unexpected_result() -> CommandError {
    tracing::warn!("agent answered with a result of the wrong shape");
    CommandError::new(
        CommandErrorKind::Internal,
        "Communication with the background engine failed.",
    )
}

/// `GetAgentStatus` over a fresh connection.
///
/// # Errors
/// A [`CommandError`] classified from the transport or server failure.
pub async fn fetch_status(locator: &AgentLocator) -> Result<AgentStatusInfo, CommandError> {
    let mut client = connect(locator).await?;
    match client.call(RequestBody::GetAgentStatus(Empty {})).await? {
        ResultBody::GetAgentStatus(status) => Ok(status),
        _ => Err(unexpected_result()),
    }
}

/// `ListDownloads` (first page, all states) over a fresh connection.
///
/// # Errors
/// A [`CommandError`] classified from the transport or server failure.
pub async fn fetch_downloads(locator: &AgentLocator) -> Result<ListDownloadsResult, CommandError> {
    let mut client = connect(locator).await?;
    let request = ListDownloadsRequest {
        states: None,
        limit: LIST_PAGE_LIMIT,
        offset: 0,
    };
    match client.call(RequestBody::ListDownloads(request)).await? {
        ResultBody::ListDownloads(result) => Ok(*result),
        _ => Err(unexpected_result()),
    }
}

/// One request over a fresh connection.
///
/// # Errors
/// A [`CommandError`] classified from the transport or server failure.
pub async fn send(locator: &AgentLocator, body: RequestBody) -> Result<ResultBody, CommandError> {
    let mut client = connect(locator).await?;
    Ok(client.call(body).await?)
}

/// `GetSnapshot`: agent status and the download list in one consistent answer.
///
/// # Errors
/// As [`send`].
pub async fn fetch_snapshot(locator: &AgentLocator) -> Result<SnapshotResult, CommandError> {
    match send(locator, RequestBody::GetSnapshot(Empty {})).await? {
        ResultBody::GetSnapshot(snapshot) => Ok(*snapshot),
        _ => Err(unexpected_result()),
    }
}

/// `GetDownload`.
///
/// # Errors
/// As [`send`].
pub async fn fetch_download(
    locator: &AgentLocator,
    body: RequestBody,
) -> Result<DownloadDetail, CommandError> {
    match send(locator, body).await? {
        ResultBody::GetDownload(detail) => Ok(*detail),
        _ => Err(unexpected_result()),
    }
}

/// `EnqueueUrl`.
///
/// # Errors
/// As [`send`].
pub async fn enqueue(
    locator: &AgentLocator,
    body: RequestBody,
) -> Result<EnqueueUrlResult, CommandError> {
    match send(locator, body).await? {
        ResultBody::EnqueueUrl(result) => Ok(result),
        _ => Err(unexpected_result()),
    }
}

/// `Pause`, `Resume`, `Cancel` or `Retry`: the resulting state.
///
/// # Errors
/// As [`send`].
pub async fn change_state(
    locator: &AgentLocator,
    body: RequestBody,
) -> Result<StateResult, CommandError> {
    match send(locator, body).await? {
        ResultBody::Pause(r)
        | ResultBody::Resume(r)
        | ResultBody::Cancel(r)
        | ResultBody::Retry(r) => Ok(r),
        _ => Err(unexpected_result()),
    }
}

/// `PauseAll`.
///
/// # Errors
/// As [`send`].
pub async fn pause_all(
    locator: &AgentLocator,
    body: RequestBody,
) -> Result<PauseAllResult, CommandError> {
    match send(locator, body).await? {
        ResultBody::PauseAll(result) => Ok(result),
        _ => Err(unexpected_result()),
    }
}

/// `ResumeAll`.
///
/// # Errors
/// As [`send`].
pub async fn resume_all(
    locator: &AgentLocator,
    body: RequestBody,
) -> Result<ResumeAllResult, CommandError> {
    match send(locator, body).await? {
        ResultBody::ResumeAll(result) => Ok(result),
        _ => Err(unexpected_result()),
    }
}

/// `UpdateLimits`: the limits now in force (also how the current limits are read).
///
/// # Errors
/// As [`send`].
pub async fn update_limits(
    locator: &AgentLocator,
    body: RequestBody,
) -> Result<UpdateLimitsResult, CommandError> {
    match send(locator, body).await? {
        ResultBody::UpdateLimits(result) => Ok(result),
        _ => Err(unexpected_result()),
    }
}

/// `UpdatePriority`.
///
/// # Errors
/// As [`send`].
pub async fn update_priority(
    locator: &AgentLocator,
    body: RequestBody,
) -> Result<(), CommandError> {
    match send(locator, body).await? {
        ResultBody::UpdatePriority(_) => Ok(()),
        _ => Err(unexpected_result()),
    }
}

/// `Remove`. A repeated remove (`not_found`) is treated as success (PLAN A.6).
///
/// # Errors
/// As [`send`], except for `not_found`.
pub async fn remove(locator: &AgentLocator, body: RequestBody) -> Result<(), CommandError> {
    let mut client = connect(locator).await?;
    match client.call(body).await {
        Ok(ResultBody::Remove(_)) => Ok(()),
        Ok(_) => Err(unexpected_result()),
        Err(IpcError::Server(error)) if error.code == ErrorCode::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pipe_name_is_local_and_user_scoped() {
        let name = default_pipe_name().expect("current user resolves");
        assert!(name.starts_with(r"\\.\pipe\ForgeDM-S-1-"), "{name}");
    }

    #[test]
    fn missing_agent_beside_the_exe_is_agent_unavailable() {
        let dir = std::env::temp_dir().join("forgedm-desktop-no-agent");
        let err = sibling_agent_path(&dir.join("forgedm-desktop.exe")).expect_err("no agent");
        assert_eq!(err.kind, CommandErrorKind::AgentUnavailable);
    }
}
