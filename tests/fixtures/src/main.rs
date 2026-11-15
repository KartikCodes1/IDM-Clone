//! ForgeDM fixture server binary: `serve --port 0` and `hash --seed S --size N`.

#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use forgedm_fixture::{FixtureError, FixtureServer, content};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "forgedm-fixture",
    about = "ForgeDM adversarial HTTP fixture server (dev only)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve on 127.0.0.1 and print the bound base URL.
    Serve {
        /// Port to bind (0 = ephemeral).
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    /// Check that the first LEN bytes of FILE match the seeded stream (prints `OK`).
    VerifyPrefix {
        /// File to check.
        #[arg(long)]
        file: std::path::PathBuf,
        /// Content seed.
        #[arg(long)]
        seed: u64,
        /// Number of leading bytes to compare.
        #[arg(long)]
        len: u64,
    },
    /// Print the SHA-256 (hex) of the first SIZE bytes of the seeded stream.
    Hash {
        /// Content seed.
        #[arg(long)]
        seed: u64,
        /// Number of bytes.
        #[arg(long)]
        size: u64,
    },
}

/// The binary's designated output functions: this is a command-line tool whose purpose is to
/// print the bound address and hashes, so the workspace print lints do not apply here.
#[allow(clippy::print_stdout, reason = "CLI output of the fixture binary")]
fn out(line: &str) {
    println!("{line}");
}

#[allow(
    clippy::print_stderr,
    reason = "CLI error output of the fixture binary"
)]
fn err(line: &str) {
    eprintln!("{line}");
}

async fn serve(port: u16) -> Result<(), FixtureError> {
    let server = FixtureServer::start_on(port).await?;
    out(&server.base_url());
    // If the signal handler cannot be installed we cannot stop gracefully; keep serving forever
    // rather than exiting immediately.
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Hash { seed, size } => {
            out(&hex::encode(content::sha256(seed, size)));
            ExitCode::SUCCESS
        }
        Command::VerifyPrefix { file, seed, len } => {
            match forgedm_fixture::verify::prefix_matches(&file, seed, len) {
                Ok(()) => {
                    out("OK");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    err(&format!("forgedm-fixture: {e}"));
                    ExitCode::FAILURE
                }
            }
        }
        Command::Serve { port } => match serve(port).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                err(&format!("forgedm-fixture: {e}"));
                ExitCode::FAILURE
            }
        },
    }
}
