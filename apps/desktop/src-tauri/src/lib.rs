//! ForgeDM desktop shell: a Tauri host whose commands relay to the agent over
//! `forgedm-ipc`. The webview never speaks the pipe protocol (ARCHITECTURE 3.7).

pub mod agent_client;
pub mod commands;
pub mod engine;
pub mod error;
pub mod logging;
pub mod open_folder;
pub mod request;

pub use commands::COMMAND_NAMES;

use commands::AgentTarget;

/// Builds and runs the application. `log_guard` is dropped (flushing the log) on the
/// exit event, because the runtime ends the process without running destructors.
///
/// # Errors
/// The Tauri runtime failed to start.
pub fn run(log_guard: logging::LogGuard) -> Result<(), tauri::Error> {
    let target = AgentTarget::new(agent_client::AgentLocator::for_current_user);
    let mut guard = Some(log_guard);
    tauri::Builder::default()
        .manage(target)
        .invoke_handler(tauri::generate_handler![
            commands::agent_status,
            commands::list_downloads,
            commands::get_snapshot,
            commands::get_download,
            commands::enqueue_url,
            commands::pause,
            commands::resume,
            commands::cancel,
            commands::retry,
            commands::remove,
            commands::start_engine,
            commands::stop_engine,
            commands::show_in_folder
        ])
        .setup(|_app| {
            tracing::info!("desktop shell started");
            Ok(())
        })
        .build(tauri::generate_context!())
        .inspect_err(|err| tracing::error!(error = %err, "the Tauri runtime failed"))?
        .run(move |_app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                drop(guard.take());
            }
        });
    Ok(())
}
