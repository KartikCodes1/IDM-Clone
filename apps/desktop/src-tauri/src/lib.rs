//! ForgeDM desktop shell: a Tauri host whose commands relay to the agent over
//! `forgedm-ipc`. The webview never speaks the pipe protocol (ARCHITECTURE 3.7).

pub mod agent_client;
pub mod commands;
pub mod error;

pub use commands::COMMAND_NAMES;

use commands::AgentTarget;

/// Builds and runs the application.
///
/// # Errors
/// The Tauri runtime failed to start.
pub fn run() -> Result<(), tauri::Error> {
    let target = AgentTarget::new(agent_client::AgentLocator::for_current_user);
    tauri::Builder::default()
        .manage(target)
        .invoke_handler(tauri::generate_handler![
            commands::agent_status,
            commands::list_downloads
        ])
        .run(tauri::generate_context!())
}
