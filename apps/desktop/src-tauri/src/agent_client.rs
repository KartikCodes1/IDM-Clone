//! Per-call agent client (Phase 0: connect, handshake, one request, drop).
//!
//! The expected agent image is the canonical `forgedm-agent.exe` next to this executable
//! (PHASE_0_1_PLAN C26); [`forgedm_ipc::Client::connect`] verifies the server's SID and
//! image path before a single byte is sent.

use std::path::{Path, PathBuf};
use std::process::Command;

use forgedm_ipc::{Client, ClientConfig};
use forgedm_protocol::{
    AgentStatusInfo, ClientKind, Empty, ListDownloadsRequest, ListDownloadsResult, RequestBody,
    ResultBody,
};

use crate::error::{CommandError, CommandErrorKind};

/// File name of the agent executable shipped next to the desktop executable.
pub const AGENT_EXE_NAME: &str = "forgedm-agent.exe";
/// Page size used for the Phase 0 list (`ListDownloads` accepts `1..=500`).
pub const LIST_PAGE_LIMIT: u32 = 500;
const CLIENT_NAME: &str = "forgedm-desktop";
/// `CREATE_NO_WINDOW`: keeps the SID lookup from flashing a console.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Upper bound on a SID string (`SECURITY_MAX_SID_STRING_CHARACTERS`).
const MAX_SID_CHARS: usize = 187;

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

    /// The production locator: the current user's default pipe and the canonical
    /// `forgedm-agent.exe` beside `current_exe()`.
    ///
    /// # Errors
    /// [`CommandErrorKind::AgentUnavailable`] when the agent executable is missing,
    /// [`CommandErrorKind::Internal`] when the user SID cannot be determined.
    pub fn for_current_user() -> Result<Self, CommandError> {
        let exe = std::env::current_exe().map_err(local_fault)?;
        let agent = sibling_agent_path(&exe)?;
        let sid = current_user_sid()?;
        Ok(Self::new(format!(r"\\.\pipe\ForgeDM-{sid}"), agent))
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

/// The current user's SID string, from `whoami /user`.
///
/// `forgedm-desktop` may depend only on `forgedm-ipc` and `forgedm-protocol`, so the
/// platform crate's SID API is out of reach. A wrong answer cannot misdirect trust: the
/// client verifies the server's SID and image path after connecting.
fn current_user_sid() -> Result<String, CommandError> {
    use std::os::windows::process::CommandExt;
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let whoami = Path::new(&system_root).join("System32").join("whoami.exe");
    let output = Command::new(whoami)
        .args(["/user", "/fo", "csv", "/nh"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(local_fault)?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_whoami_sid(&text).ok_or_else(|| {
        CommandError::new(
            CommandErrorKind::Internal,
            "The current user could not be determined.",
        )
    })
}

/// Extracts the SID from `"DOMAIN\user","S-1-5-21-..."`.
fn parse_whoami_sid(csv: &str) -> Option<String> {
    let sid = csv.trim().rsplit(',').next()?.trim().trim_matches('"');
    let valid = sid.starts_with("S-1-")
        && sid.len() <= MAX_SID_CHARS
        && sid
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S');
    valid.then(|| sid.to_owned())
}

async fn connect(locator: &AgentLocator) -> Result<Client, CommandError> {
    let config = ClientConfig::new(&locator.pipe_name, &locator.expected_agent_path);
    let mut client = Client::connect(config).await?;
    client
        .handshake(ClientKind::Desktop, CLIENT_NAME, env!("CARGO_PKG_VERSION"))
        .await?;
    Ok(client)
}

fn unexpected_result() -> CommandError {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whoami_csv_yields_the_sid() {
        let csv = "\"DESKTOP\\me\",\"S-1-5-21-1-2-3-1001\"\r\n";
        assert_eq!(
            parse_whoami_sid(csv).as_deref(),
            Some("S-1-5-21-1-2-3-1001")
        );
    }

    #[test]
    fn malformed_whoami_output_is_rejected() {
        assert_eq!(parse_whoami_sid(""), None);
        assert_eq!(parse_whoami_sid("\"a\\b\",\"not-a-sid\""), None);
        assert_eq!(parse_whoami_sid("\"a\",\"S-1-5-21\\..\\evil\""), None);
    }

    #[test]
    fn missing_agent_beside_the_exe_is_agent_unavailable() {
        let dir = std::env::temp_dir().join("forgedm-desktop-no-agent");
        let err = sibling_agent_path(&dir.join("forgedm-desktop.exe")).expect_err("no agent");
        assert_eq!(err.kind, CommandErrorKind::AgentUnavailable);
    }
}
