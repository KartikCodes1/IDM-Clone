//! ForgeDM command-line client (`forgedm`).
//!
//! Commands: `status`, `list [--json]`, `agent stop`, with a global `--data-dir`.
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
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            output::error_line(&err.to_string());
            ExitCode::from(err.exit_code())
        }
    }
}

/// Dispatches one parsed command line.
async fn run(cli: Cli) -> Result<(), error::CliError> {
    let target = client::Target::resolve(cli.data_dir.as_deref())?;
    match cli.command {
        Command::Status => commands::status::run(&target).await,
        Command::List { json } => commands::list::run(&target, json).await,
        Command::Agent(action) => commands::agent::run(&target, action).await,
    }
}
