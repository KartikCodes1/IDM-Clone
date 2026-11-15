//! Command-line grammar (clap).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// ForgeDM command-line client.
#[derive(Debug, Parser)]
#[command(name = "forgedm", version, about)]
pub struct Cli {
    /// Absolute data directory of the agent to talk to (default: `%LOCALAPPDATA%\ForgeDM`).
    #[arg(long, global = true, value_name = "ABS_DIR", value_parser = absolute_path)]
    pub data_dir: Option<PathBuf>,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands. `add` and `watch` arrive with S1.7.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the agent's status without starting it.
    Status,
    /// List downloads.
    List {
        /// Print the list as one JSON object `{"items": [...], "total": n}`.
        #[arg(long)]
        json: bool,
    },
    /// Control the background agent.
    #[command(subcommand)]
    Agent(AgentCommand),
}

/// `forgedm agent ...`.
#[derive(Debug, Clone, Copy, Subcommand)]
pub enum AgentCommand {
    /// Ask the agent to drain and exit.
    Stop,
}

/// Accepts only absolute paths, like the agent's own `--data-dir`.
fn absolute_path(raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("the data directory must be an absolute path".to_owned())
    }
}
