//! Connecting to the agent: pipe name, expected agent path, connect and handshake.

use std::path::{Path, PathBuf};

use forgedm_ipc::{Client, ClientConfig, IpcError, ensure_agent, pipe_name_for};
use forgedm_protocol::{ClientKind, HandshakeResult};

use crate::error::CliError;

/// File name of the agent binary installed next to the CLI (PLAN A.10).
const AGENT_FILE_NAME: &str = "forgedm-agent.exe";
/// Name sent in the handshake.
const CLIENT_NAME: &str = "forgedm-cli";

/// Which agent a command talks to.
#[derive(Debug, Clone)]
pub struct Target {
    pipe_name: String,
    expected_agent: PathBuf,
    data_dir: Option<PathBuf>,
}

impl Target {
    /// Resolves the pipe name for `data_dir` (default root when `None`) and the expected
    /// agent: the canonical sibling `forgedm-agent.exe` of the running executable.
    ///
    /// # Errors
    /// [`CliError::Ipc`] when the user SID, the default root or the executable path cannot
    /// be resolved.
    pub fn resolve(data_dir: Option<&Path>) -> Result<Self, CliError> {
        let pipe_name =
            pipe_name_for(data_dir).map_err(|err| CliError::Ipc(IpcError::Platform(err)))?;
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|err| CliError::Ipc(IpcError::Io(err)))?;
        Ok(Self {
            pipe_name,
            expected_agent: sibling_agent(&exe),
            data_dir: data_dir.map(Path::to_path_buf),
        })
    }

    /// Connects (never starting the agent), verifies the server and handshakes.
    ///
    /// # Errors
    /// [`CliError::NotRunning`], [`CliError::IdentityFailed`], [`CliError::ProtocolMismatch`]
    /// or another transport error.
    pub async fn connect(&self) -> Result<(Client, HandshakeResult), CliError> {
        let config = ClientConfig::new(self.pipe_name.clone(), self.expected_agent.clone());
        let client = Client::connect(config).await?;
        Self::hello(client).await
    }

    /// Connects, starting the agent first when it is not running (`ensure_agent`, ADR-005),
    /// verifies the server and handshakes. The agent started is only ever the sibling
    /// `forgedm-agent.exe`.
    ///
    /// # Errors
    /// [`CliError::Bootstrap`] (exit 15) when the agent cannot be started or does not come
    /// up in time; connection failures keep their own mapping.
    pub async fn connect_or_start(&self) -> Result<(Client, HandshakeResult), CliError> {
        let client = ensure_agent(&self.expected_agent, self.data_dir.as_deref()).await?;
        Self::hello(client).await
    }

    async fn hello(mut client: Client) -> Result<(Client, HandshakeResult), CliError> {
        let hello = client
            .handshake(ClientKind::Cli, CLIENT_NAME, env!("CARGO_PKG_VERSION"))
            .await?;
        Ok((client, hello))
    }
}

/// The agent binary next to `exe`, canonical when it exists.
fn sibling_agent(exe: &Path) -> PathBuf {
    let sibling = exe.with_file_name(AGENT_FILE_NAME);
    std::fs::canonicalize(&sibling).unwrap_or(sibling)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_agent_replaces_the_file_name() {
        let path = sibling_agent(Path::new(r"C:\no\such\dir\forgedm.exe"));

        assert_eq!(path, PathBuf::from(r"C:\no\such\dir\forgedm-agent.exe"));
    }
}
