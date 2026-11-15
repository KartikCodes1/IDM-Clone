//! Tauri commands exposed to the webview. Each is ACL-gated through the app manifest in
//! `build.rs` and granted only by `capabilities/main-window.json` (ADR-022).

use forgedm_protocol::{AgentStatusInfo, ListDownloadsResult};
use std::sync::Mutex;

use tauri::State;

use crate::agent_client::{self, AgentLocator};
use crate::error::CommandError;

/// The registered command names; must equal the `build.rs` manifest list and the
/// `generate_handler!` entries (checked by `manifest_matches_registered_commands`).
pub const COMMAND_NAMES: [&str; 2] = ["agent_status", "list_downloads"];

/// Builds the locator; replaceable so tests can simulate a startup failure.
pub type Resolver = fn() -> Result<AgentLocator, CommandError>;

/// Managed state: resolves the agent locator lazily and caches only a success, so a
/// missing agent executable or a transient lookup failure recovers on a later call.
#[derive(Debug)]
pub struct AgentTarget {
    resolver: Resolver,
    cached: Mutex<Option<AgentLocator>>,
}

impl AgentTarget {
    /// A target that resolves the locator on first successful use.
    #[must_use]
    pub fn new(resolver: Resolver) -> Self {
        Self {
            resolver,
            cached: Mutex::new(None),
        }
    }

    /// The cached locator, resolving it now when none is cached yet.
    ///
    /// # Errors
    /// The resolver's failure; nothing is cached in that case.
    pub fn locator(&self) -> Result<AgentLocator, CommandError> {
        let mut guard = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = guard.as_ref() {
            return Ok(found.clone());
        }
        let resolved = (self.resolver)()?;
        *guard = Some(resolved.clone());
        Ok(resolved)
    }
}

/// Current agent status, fetched over a fresh pipe connection.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the pipe is absent).
#[tauri::command]
pub async fn agent_status(target: State<'_, AgentTarget>) -> Result<AgentStatusInfo, CommandError> {
    agent_client::fetch_status(&target.locator()?).await
}

/// The download list (first page), fetched over a fresh pipe connection.
///
/// # Errors
/// [`CommandError`] (`agent_unavailable` when the pipe is absent).
#[tauri::command]
pub async fn list_downloads(
    target: State<'_, AgentTarget>,
) -> Result<ListDownloadsResult, CommandError> {
    agent_client::fetch_downloads(&target.locator()?).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CommandErrorKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    fn fails_once_then_succeeds() -> Result<AgentLocator, CommandError> {
        if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(CommandError::new(
                CommandErrorKind::AgentUnavailable,
                "missing",
            ))
        } else {
            Ok(AgentLocator::new("pipe", "agent.exe"))
        }
    }

    #[test]
    fn locator_failure_is_not_cached_and_success_is() {
        let target = AgentTarget::new(fails_once_then_succeeds);
        assert!(target.locator().is_err());
        assert!(target.locator().is_ok(), "recovers after the first failure");
        assert!(target.locator().is_ok());
        assert_eq!(CALLS.load(Ordering::SeqCst), 2, "success is cached");
    }
}
