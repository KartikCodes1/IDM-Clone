//! The desktop's per-call agent client (no real agent is needed for these).

#![allow(clippy::expect_used)] // test code may expect (ARCHITECTURE 1.4)

use std::path::PathBuf;

use forgedm_desktop_lib::agent_client::{AgentLocator, fetch_downloads, fetch_status};
use forgedm_desktop_lib::error::CommandErrorKind;

fn absent_agent() -> AgentLocator {
    let name = format!(
        r"\\.\pipe\ForgeDM-desktop-test-absent-{}",
        std::process::id()
    );
    AgentLocator::new(name, PathBuf::from(r"C:\nonexistent\forgedm-agent.exe"))
}

#[tokio::test]
async fn agent_status_maps_absent_pipe_to_agent_unavailable() {
    let err = fetch_status(&absent_agent())
        .await
        .expect_err("no agent is listening");
    assert_eq!(err.kind, CommandErrorKind::AgentUnavailable);
    assert!(!err.message.is_empty());
}

#[tokio::test]
async fn list_downloads_maps_absent_pipe_to_agent_unavailable() {
    let err = fetch_downloads(&absent_agent())
        .await
        .expect_err("no agent is listening");
    assert_eq!(err.kind, CommandErrorKind::AgentUnavailable);
}

#[tokio::test]
async fn non_local_pipe_name_is_rejected_without_connecting() {
    let locator = AgentLocator::new(r"\\remote\pipe\x", PathBuf::from(r"C:\x.exe"));
    let err = fetch_status(&locator)
        .await
        .expect_err("remote pipes are refused");
    assert_eq!(err.kind, CommandErrorKind::Internal);
}
