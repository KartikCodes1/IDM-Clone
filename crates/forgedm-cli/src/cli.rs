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
    /// Show or change the agent-wide limits (stored; applied at once).
    Limits(LimitsArgs),
    /// Pause every download that can be paused.
    PauseAll {
        /// Also pause downloads that cannot resume, throwing their progress away.
        #[arg(long)]
        discard_progress: bool,
    },
    /// Resume every paused download.
    ResumeAll,
    /// Change a download's queue priority.
    Priority {
        /// The download id.
        id: Uuid,
        /// The new priority.
        priority: PriorityArg,
    },
    /// Control the background agent.
    #[command(subcommand)]
    Agent(AgentCommand),
}

/// `forgedm limits [--max-active N] [--speed <bytes/s|off>] [--connections N]`.
#[derive(Debug, Clone, Copy, Args)]
pub struct LimitsArgs {
    /// Downloads that run at once (1 to 10).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10))]
    pub max_active: Option<u32>,
    /// Total speed limit in bytes per second (at least 1024), or `off`.
    #[arg(long, value_name = "BYTES_PER_SEC|off", value_parser = speed_limit)]
    pub speed: Option<SpeedLimit>,
    /// Default connections for new downloads (1 to 16).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=16))]
    pub connections: Option<u32>,
}

/// A speed limit: none, or a number of bytes per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedLimit {
    /// No limit.
    Off,
    /// At most this many bytes per second.
    BytesPerSecond(u64),
}

/// The priorities a user can choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PriorityArg {
    /// Runs before everything else.
    High,
    /// The default.
    Normal,
    /// Runs after everything else.
    Low,
}

/// Smallest speed limit the agent accepts (A.3).
const MIN_SPEED_BPS: u64 = 1024;

/// Parses `off` or a byte rate of at least [`MIN_SPEED_BPS`].
fn speed_limit(raw: &str) -> Result<SpeedLimit, String> {
    if raw.eq_ignore_ascii_case("off") {
        return Ok(SpeedLimit::Off);
    }
    let bytes: u64 = raw
        .parse()
        .map_err(|_| "expected a number of bytes per second, or `off`".to_owned())?;
    if bytes < MIN_SPEED_BPS {
        return Err(format!(
            "the limit must be at least {MIN_SPEED_BPS} bytes per second"
        ));
    }
    Ok(SpeedLimit::BytesPerSecond(bytes))
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
    fn limits_parse_each_flag_and_off() {
        let parse = |args: &[&str]| {
            let mut full = vec!["forgedm", "limits"];
            full.extend_from_slice(args);
            Cli::try_parse_from(full)
        };
        let Command::Limits(args) = parse(&[
            "--max-active",
            "2",
            "--speed",
            "500000",
            "--connections",
            "6",
        ])
        .unwrap()
        .command
        else {
            unreachable!("not limits");
        };
        assert_eq!(args.max_active, Some(2));
        assert_eq!(args.speed, Some(SpeedLimit::BytesPerSecond(500_000)));
        assert_eq!(args.connections, Some(6));
        let Command::Limits(off) = parse(&["--speed", "OFF"]).unwrap().command else {
            unreachable!("not limits");
        };
        assert_eq!(off.speed, Some(SpeedLimit::Off));
        let Command::Limits(none) = parse(&[]).unwrap().command else {
            unreachable!("not limits");
        };
        assert!(none.max_active.is_none() && none.speed.is_none() && none.connections.is_none());
    }

    #[test]
    fn limits_refuse_values_outside_the_documented_ranges() {
        let bad = |flag: &str, value: &str| {
            Cli::try_parse_from(["forgedm", "limits", flag, value]).is_err()
        };
        assert!(bad("--max-active", "0"));
        assert!(bad("--max-active", "11"));
        assert!(bad("--connections", "0"));
        assert!(bad("--connections", "17"));
        assert!(bad("--speed", "1023"));
        assert!(bad("--speed", "fast"));
        assert!(bad("--speed", "-5"));
    }

    #[test]
    fn batch_and_priority_commands_parse() {
        let id = Uuid::nil().to_string();
        assert!(matches!(
            Cli::try_parse_from(["forgedm", "pause-all", "--discard-progress"])
                .unwrap()
                .command,
            Command::PauseAll {
                discard_progress: true
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["forgedm", "resume-all"])
                .unwrap()
                .command,
            Command::ResumeAll
        ));
        for (word, expected) in [
            ("high", PriorityArg::High),
            ("normal", PriorityArg::Normal),
            ("low", PriorityArg::Low),
        ] {
            let cli = Cli::try_parse_from(["forgedm", "priority", &id, word]).unwrap();
            assert!(matches!(
                cli.command,
                Command::Priority { priority, .. } if priority == expected
            ));
        }
        assert!(Cli::try_parse_from(["forgedm", "priority", &id, "urgent"]).is_err());
        assert!(Cli::try_parse_from(["forgedm", "priority", &id]).is_err());
    }

    #[test]
    fn watch_takes_a_download_id() {
        let id = Uuid::nil().to_string();
        let cli = Cli::try_parse_from(["forgedm", "watch", &id]).unwrap();
        assert!(matches!(cli.command, Command::Watch { .. }));
        assert!(Cli::try_parse_from(["forgedm", "watch", "not-a-uuid"]).is_err());
    }
}
