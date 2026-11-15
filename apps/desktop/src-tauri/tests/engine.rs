//! Engine start/stop use the forgedm-ipc bootstrap with the sibling agent path.

#![allow(clippy::expect_used)] // test code may expect (ARCHITECTURE 1.4)

use std::path::PathBuf;

use forgedm_desktop_lib::agent_client::AgentLocator;
use forgedm_desktop_lib::engine::{start_engine, stop_engine};
use forgedm_desktop_lib::error::CommandErrorKind;

fn unique_pipe(tag: &str) -> String {
    format!(
        r"\\.\pipe\ForgeDM-desktop-engine-test-{tag}-{}",
        std::process::id()
    )
}

#[tokio::test]
async fn start_engine_uses_sibling_bootstrap() {
    // The bootstrap refuses a missing agent image before spawning anything; the error is
    // the engine-not-installed class, proving the locator's sibling path is what is used.
    let missing = PathBuf::from(r"C:\forgedm-test-no-such-dir\forgedm-agent.exe");
    let locator = AgentLocator::new(unique_pipe("missing"), missing);
    let err = start_engine(&locator, None)
        .await
        .expect_err("agent not installed");
    assert_eq!(err.kind, CommandErrorKind::AgentUnavailable);
    assert!(!err.message.contains("no-such-dir"), "paths are not echoed");
}

#[tokio::test]
async fn start_engine_rejects_a_relative_agent_path_without_spawning() {
    let locator = AgentLocator::new(unique_pipe("relative"), PathBuf::from("forgedm-agent.exe"));
    let err = start_engine(&locator, None)
        .await
        .expect_err("relative path");
    assert_eq!(err.kind, CommandErrorKind::Internal);
}

#[tokio::test]
async fn stop_engine_without_an_agent_is_agent_unavailable() {
    let locator = AgentLocator::new(
        unique_pipe("stop"),
        PathBuf::from(r"C:\x\forgedm-agent.exe"),
    );
    let err = stop_engine(&locator).await.expect_err("nothing to stop");
    assert_eq!(err.kind, CommandErrorKind::AgentUnavailable);
}
