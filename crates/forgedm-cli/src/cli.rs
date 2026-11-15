//! Command-line grammar (clap).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use uuid::Uuid;

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

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Add a download (starting the agent when it is not running) and optionally follow it.
    Add(AddArgs),
    /// Follow a download until it ends: bytes, speed and ETA, then the result.
    Watch {
        /// The download id printed by `add` or `list`.
        id: Uuid,
    },
    /// Show the agent's status without starting it.
    Status,
    /// List downloads.
    List {
        /// Print the list as one JSON object `{"items": [...], "total": n}`.
        #[arg(long)]
        json: bool,
    },
    /// Show one download in detail (state, true bytes, resume support, segments).
    #[command(visible_alias = "show")]
    Get {
        /// The download id.
        id: Uuid,
        /// Print the detail as one JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Pause a download. It checkpoints first; `resume` continues from the committed bytes.
    Pause {
        /// The download id.
        id: Uuid,
        /// Allow pausing a download that cannot resume, throwing its progress away.
        #[arg(long)]
        discard_progress: bool,
    },
    /// Resume a paused or interrupted download.
    Resume {
        /// The download id.
        id: Uuid,
    },
    /// Cancel a download; its partial file is kept.
    Cancel {
        /// The download id.
        id: Uuid,
    },
    /// Retry a failed or cancelled download.
    Retry {
        /// The download id.
        id: Uuid,
        /// Forget progress and the remote file's identity and start over.
        #[arg(long)]
        restart_from_zero: bool,
    },
    /// Remove a download from the list (a completed file is never deleted).
    Remove {
        /// The download id.
        id: Uuid,
        /// Also delete the partial file ForgeDM created for it.
        #[arg(long)]
        delete_partial: bool,
        /// Confirm deleting the partial file without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Control the background agent.
    #[command(subcommand)]
    Agent(AgentCommand),
}

/// `forgedm add <url|-> ...`.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// The URL to download, or `-` to read it from standard input (keeps a token-bearing
    /// URL out of the process list and shell history).
    pub url: String,
    /// Destination directory (default: the Downloads folder).
    #[arg(long, value_name = "DIR", value_parser = absolute_from_cwd)]
    pub dir: Option<PathBuf>,
    /// File name to use instead of the server's.
    #[arg(long, value_name = "NAME")]
    pub file_name: Option<String>,
    /// Expected SHA-256 (64 hex characters); a mismatch fails the download.
    #[arg(long, value_name = "HEX", value_parser = sha256_hex)]
    pub sha256: Option<String>,
    /// Follow the download until it ends; exit 13 if it fails or is cancelled.
    #[arg(long)]
    pub watch: bool,
    /// Do not start the agent when it is not running (exit 10 instead).
    #[arg(long)]
    pub no_start: bool,
    /// Most connections to open for this download (1 to 16; default 8).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=16))]
    pub connections: Option<u32>,
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

/// Resolves a relative `--dir` against the current directory, so the agent always gets an
/// absolute path.
fn absolute_from_cwd(raw: &str) -> Result<PathBuf, String> {
    std::path::absolute(raw).map_err(|err| format!("cannot resolve the directory: {err}"))
}

/// Accepts exactly 64 hex digits and normalises them to lowercase.
fn sha256_hex(raw: &str) -> Result<String, String> {
    if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(raw.to_ascii_lowercase())
    } else {
        Err("expected 64 hexadecimal characters".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_validated_and_lowercased() {
        assert_eq!(sha256_hex(&"AB".repeat(32)).unwrap(), "ab".repeat(32));
        assert!(sha256_hex("abc").is_err());
        assert!(sha256_hex(&"g".repeat(64)).is_err());
    }

    #[test]
    fn a_relative_directory_becomes_absolute() {
        assert!(absolute_from_cwd("downloads").unwrap().is_absolute());
    }

    #[test]
    fn add_parses_every_flag() {
        let cli = Cli::try_parse_from([
            "forgedm",
            "add",
            "-",
            "--dir",
            r"C:\d",
            "--file-name",
            "a.bin",
            "--watch",
            "--no-start",
        ])
        .unwrap();
        let Command::Add(args) = cli.command else {
            unreachable!("not add");
        };
        assert_eq!(args.url, "-");
        assert!(args.watch && args.no_start);
        assert_eq!(args.file_name.as_deref(), Some("a.bin"));
    }

    #[test]
    fn connections_flag_takes_1_to_16_only() {
        let parse =
            |n: &str| Cli::try_parse_from(["forgedm", "add", "https://a/b", "--connections", n]);
        let Command::Add(args) = parse("8").unwrap().command else {
            unreachable!("not add");
        };
        assert_eq!(args.connections, Some(8));
        assert!(parse("0").is_err());
        assert!(parse("17").is_err());
        assert!(parse("x").is_err());
        let Command::Add(args) = Cli::try_parse_from(["forgedm", "add", "https://a/b"])
            .unwrap()
            .command
        else {
            unreachable!("not add");
        };
        assert_eq!(args.connections, None, "the agent picks the default");
    }

    #[test]
    fn control_commands_parse_their_flags() {
        let id = Uuid::nil().to_string();
        let cli =
            Cli::try_parse_from(["forgedm", "remove", &id, "--delete-partial", "--yes"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Remove {
                delete_partial: true,
                yes: true,
                ..
            }
        ));
        let cli = Cli::try_parse_from(["forgedm", "pause", &id, "--discard-progress"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Pause {
                discard_progress: true,
                ..
            }
        ));
        let cli = Cli::try_parse_from(["forgedm", "retry", &id, "--restart-from-zero"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Retry {
                restart_from_zero: true,
                ..
            }
        ));
        let cli = Cli::try_parse_from(["forgedm", "show", &id, "--json"]).unwrap();
        assert!(
            matches!(cli.command, Command::Get { json: true, .. }),
            "show is an alias of get"
        );
        assert!(
            Cli::try_parse_from(["forgedm", "cancel"]).is_err(),
            "the id is required"
        );
    }

    #[test]
    fn watch_takes_a_download_id() {
        let id = Uuid::nil().to_string();
        let cli = Cli::try_parse_from(["forgedm", "watch", &id]).unwrap();
        assert!(matches!(cli.command, Command::Watch { .. }));
        assert!(Cli::try_parse_from(["forgedm", "watch", "not-a-uuid"]).is_err());
    }
}
