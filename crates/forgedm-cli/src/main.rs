//! ForgeDM command-line client (`forgedm`).
//!
//! Commands: `add`, `watch`, `status`, `list [--json]`, `get|show <id> [--json]`, `pause`,
//! `resume`, `cancel`, `retry`, `remove`, `limits`, `pause-all`, `resume-all`, `priority`,
//! `agent stop`, with a global `--data-dir`.
//! Exit codes follow PHASE_0_1_PLAN Appendix A.8 (see [`error::CliError::exit_code`]).

#![forbid(unsafe_code)]

mod cli;
mod client;
mod commands;
mod error;
mod output;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};

/// Exit code for a clap usage error (A.8: 2). Help and version exit 0.
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let code = if err.use_stderr() { EXIT_USAGE } else { 0 };
            output::clap_error(&err);
            return ExitCode::from(code);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            output::error_line(&format!("cannot start the async runtime: {err}"));
            return ExitCode::from(error::EXIT_INTERNAL);
        }
    };
    let code = match runtime.block_on(run(cli)) {
        Ok(()) => 0,
        Err(err) => {
            output::error_line(&err.to_string());
            err.exit_code()
        }
    };
    ExitCode::from(output::finish(code))
}

/// Dispatches one parsed command line.
async fn run(cli: Cli) -> Result<(), error::CliError> {
    let target = client::Target::resolve(cli.data_dir.as_deref())?;
    match cli.command {
        Command::Add(args) => commands::add::run(&target, args).await,
        Command::Watch { id } => commands::watch::run(&target, id).await,
        Command::Status => commands::status::run(&target).await,
        Command::List { json } => commands::list::run(&target, json).await,
        Command::Get { id, json } => commands::get::run(&target, id, json).await,
        Command::Pause {
            id,
            discard_progress,
        } => commands::control::pause(&target, id, discard_progress).await,
        Command::Resume { id } => commands::control::resume(&target, id).await,
        Command::Cancel { id } => commands::control::cancel(&target, id).await,
        Command::Retry {
            id,
            restart_from_zero,
        } => commands::control::retry(&target, id, restart_from_zero).await,
        Command::Remove {
            id,
            delete_partial,
            yes,
        } => commands::control::remove(&target, id, delete_partial, yes).await,
        Command::Limits(args) => commands::limits::limits(&target, args).await,
        Command::PauseAll { discard_progress } => {
            commands::limits::pause_all(&target, discard_progress).await
        }
        Command::ResumeAll => commands::limits::resume_all(&target).await,
        Command::Priority { id, priority } => {
            commands::limits::priority(&target, id, priority).await
        }
        Command::Agent(action) => commands::agent::run(&target, action).await,
    }
}
