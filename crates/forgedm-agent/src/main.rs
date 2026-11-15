//! ForgeDM agent binary. Windows subsystem in release builds, console in debug builds.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod output;

use std::process::ExitCode;

use forgedm_agent::app::{self, request_shutdown};
use forgedm_agent::config::Config;
use forgedm_agent::error::{StartupError, exit};
use forgedm_agent::events_hub::HubHandle;
use forgedm_agent::logging;
use tokio_util::sync::CancellationToken;

fn main() -> ExitCode {
    ExitCode::from(run())
}

/// Runs the agent and returns its exit code (PHASE_0_1_PLAN A.8).
fn run() -> u8 {
    let config = match Config::parse(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(err) => return fail(&StartupError::Usage(err)),
    };
    logging::install_panic_hook();
    let prepared = match app::prepare(config) {
        Ok(prepared) => prepared,
        Err(err) => return fail(&err),
    };
    // Held until this function returns so the last log lines are flushed.
    let _log_guard = match logging::init(prepared.logs_dir(), prepared.log_filter()) {
        Ok(guard) => guard,
        Err(err) => return fail(&err),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return fail(&StartupError::Internal(err.to_string())),
    };
    match runtime.block_on(serve(prepared)) {
        Ok(()) => exit::OK,
        Err(err) => fail(&err),
    }
}

async fn serve(prepared: app::Prepared) -> Result<(), StartupError> {
    let shutdown = CancellationToken::new();
    let agent = app::start(prepared, shutdown.clone()).await?;
    tokio::spawn(shutdown_on_ctrl_c(agent.hub().clone(), shutdown));
    agent.wait().await
}

/// Console builds: Ctrl+C is a graceful shutdown. A windowless process never receives it.
async fn shutdown_on_ctrl_c(hub: HubHandle, shutdown: CancellationToken) {
    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            match signal {
                Ok(()) => {
                    tracing::info!("Ctrl+C received; shutting down");
                    request_shutdown(&hub, &shutdown).await;
                }
                Err(err) => tracing::warn!(
                    error = %err,
                    "Ctrl+C handler failed; graceful Ctrl+C shutdown unavailable"
                ),
            }
        }
        () = shutdown.cancelled() => {}
    }
}

/// Failures before `logging::init` (usage 2, lock 3, data dir 6) have no log sink: the only
/// logs directory lives inside the data directory that is still being validated. They are
/// reported by exit code (PHASE_0_1_PLAN A.8) and, in console builds, on stderr.
///
/// Logs the failure, shows it in a console build and returns its exit code.
fn fail(err: &StartupError) -> u8 {
    tracing::error!(error = %err, code = err.exit_code(), "agent failed");
    output::error_line(&err.to_string());
    err.exit_code()
}
