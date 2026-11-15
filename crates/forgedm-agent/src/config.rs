//! Command-line configuration and the pipe name derivation (PHASE_0_1_PLAN S0.8, A.10).
//!
//! Flags: `--data-dir <abs>`, `--proxy system|none` (default `system`),
//! `--log-level <filter>` (default `info`). Parsing is hand-written: the agent has three
//! flags and `clap` is not a dependency of this crate.

use std::path::{Path, PathBuf};

pub use forgedm_ipc::paths::default_data_root;
use forgedm_ipc::paths::pipe_name_for;
use forgedm_platform::PlatformError;
use forgedm_protocol::ProxySource;
use tracing_subscriber::EnvFilter;

/// Log filter used when `--log-level` is absent.
pub const DEFAULT_LOG_FILTER: &str = "info";
/// Environment variables that make the system proxy source "environment".
const PROXY_ENV_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

/// Proxy routing requested with `--proxy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// Use the system or environment proxy settings.
    System,
    /// Connect directly.
    None,
}

/// A command-line problem. The agent exits with code 2.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// An argument that is not a known flag.
    #[error("unknown argument: {0}")]
    UnknownArgument(String),
    /// A flag that needs a value was last.
    #[error("{0} needs a value")]
    MissingValue(&'static str),
    /// `--proxy` was neither `system` nor `none`.
    #[error("--proxy must be `system` or `none`")]
    InvalidProxy,
    /// `--data-dir` was not an absolute path.
    #[error("--data-dir must be an absolute path")]
    RelativeDataDir,
    /// `--log-level` is not a valid filter.
    #[error("--log-level is not a valid filter")]
    InvalidLogFilter,
    /// A flag was given twice.
    #[error("{0} was given more than once")]
    Duplicate(&'static str),
}

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Explicit data root; `None` means `%LOCALAPPDATA%\ForgeDM`.
    pub data_dir: Option<PathBuf>,
    /// Requested proxy routing.
    pub proxy: ProxyMode,
    /// `tracing` filter directive string.
    pub log_filter: String,
}

impl Config {
    /// A config with an explicit data root (tests and embedding).
    #[must_use]
    pub fn new(data_dir: PathBuf, proxy: ProxyMode, log_filter: String) -> Self {
        Self {
            data_dir: Some(data_dir),
            proxy,
            log_filter,
        }
    }

    /// Parses the arguments after the program name.
    ///
    /// # Errors
    /// [`ConfigError`] for anything that is not a well-formed flag list.
    pub fn parse<I>(args: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = String>,
    {
        let mut data_dir = None;
        let mut proxy = None;
        let mut log_filter = None;
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--data-dir" => set_once(&mut data_dir, "--data-dir", args.next())?,
                "--proxy" => set_once(&mut proxy, "--proxy", args.next())?,
                "--log-level" => set_once(&mut log_filter, "--log-level", args.next())?,
                _ => return Err(ConfigError::UnknownArgument(flag)),
            }
        }
        let data_dir = data_dir.map(PathBuf::from);
        if data_dir.as_deref().is_some_and(|d| !d.is_absolute()) {
            return Err(ConfigError::RelativeDataDir);
        }
        let log_filter = log_filter.unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned());
        EnvFilter::try_new(&log_filter).map_err(|_| ConfigError::InvalidLogFilter)?;
        Ok(Self {
            data_dir,
            proxy: parse_proxy(proxy.as_deref())?,
            log_filter,
        })
    }

    /// The data root: the explicit one, or `%LOCALAPPDATA%\ForgeDM` from the shell.
    ///
    /// # Errors
    /// [`PlatformError`] when the known folder cannot be resolved.
    pub fn resolve_data_root(&self) -> Result<PathBuf, PlatformError> {
        match &self.data_dir {
            Some(dir) => Ok(dir.clone()),
            None => default_data_root(),
        }
    }
}

fn set_once(
    slot: &mut Option<String>,
    flag: &'static str,
    value: Option<String>,
) -> Result<(), ConfigError> {
    if slot.is_some() {
        return Err(ConfigError::Duplicate(flag));
    }
    *slot = Some(value.ok_or(ConfigError::MissingValue(flag))?);
    Ok(())
}

fn parse_proxy(value: Option<&str>) -> Result<ProxyMode, ConfigError> {
    match value {
        None | Some("system") => Ok(ProxyMode::System),
        Some("none") => Ok(ProxyMode::None),
        Some(_) => Err(ConfigError::InvalidProxy),
    }
}

/// The proxy source to report. `lookup` reads one environment variable.
///
/// `--proxy none` is `none`. `--proxy system` is `environment` when a proxy variable is set
/// and non-empty (reqwest reads those first), otherwise `system`.
#[must_use]
pub fn effective_proxy_source(
    mode: ProxyMode,
    lookup: impl Fn(&str) -> Option<String>,
) -> ProxySource {
    match mode {
        ProxyMode::None => ProxySource::None,
        ProxyMode::System => {
            let from_env = PROXY_ENV_VARS
                .iter()
                .any(|name| lookup(name).is_some_and(|v| !v.trim().is_empty()));
            if from_env {
                ProxySource::Environment
            } else {
                ProxySource::System
            }
        }
    }
}

/// The pipe name of the agent serving `data_dir` (`None` = the default root) for the
/// current user (A.10). Delegates to the helper shared with the clients, so the agent and
/// every client derive the same name.
///
/// # Errors
/// [`PlatformError`] when the user SID or the default root cannot be resolved.
pub fn pipe_name(data_dir: Option<&Path>) -> Result<String, PlatformError> {
    pipe_name_for(data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn defaults_are_system_proxy_info_level_default_root() {
        let config = Config::parse(args(&[])).unwrap();
        assert_eq!(config.data_dir, None);
        assert_eq!(config.proxy, ProxyMode::System);
        assert_eq!(config.log_filter, "info");
    }

    #[test]
    fn all_flags_parse() {
        let config = Config::parse(args(&[
            "--data-dir",
            r"C:\temp\forge",
            "--proxy",
            "none",
            "--log-level",
            "debug,forgedm_agent=trace",
        ]))
        .unwrap();
        assert_eq!(config.data_dir, Some(PathBuf::from(r"C:\temp\forge")));
        assert_eq!(config.proxy, ProxyMode::None);
        assert_eq!(config.log_filter, "debug,forgedm_agent=trace");
    }

    #[test]
    fn relative_data_dir_is_a_usage_error() {
        for dir in [
            "data",
            r".\data",
            r"..\data",
            r"\rooted",
            "C:drive-relative",
        ] {
            let err = Config::parse(args(&["--data-dir", dir])).unwrap_err();
            assert_eq!(err, ConfigError::RelativeDataDir, "{dir}");
        }
    }

    #[test]
    fn malformed_command_lines_are_usage_errors() {
        assert_eq!(
            Config::parse(args(&["--proxy", "pac"])),
            Err(ConfigError::InvalidProxy)
        );
        assert_eq!(
            Config::parse(args(&["--data-dir"])),
            Err(ConfigError::MissingValue("--data-dir"))
        );
        assert_eq!(
            Config::parse(args(&["--bogus"])),
            Err(ConfigError::UnknownArgument("--bogus".to_owned()))
        );
        assert_eq!(
            Config::parse(args(&["--proxy", "none", "--proxy", "system"])),
            Err(ConfigError::Duplicate("--proxy"))
        );
        assert_eq!(
            Config::parse(args(&["--log-level", "not[a[filter"])),
            Err(ConfigError::InvalidLogFilter)
        );
    }

    #[test]
    fn proxy_source_follows_mode_and_environment() {
        let none = |_: &str| None;
        let with = |name: &str| (name == "HTTPS_PROXY").then(|| "http://p:1".to_owned());
        let blank = |name: &str| (name == "http_proxy").then(|| "  ".to_owned());
        assert_eq!(
            effective_proxy_source(ProxyMode::None, with),
            ProxySource::None
        );
        assert_eq!(
            effective_proxy_source(ProxyMode::System, none),
            ProxySource::System
        );
        assert_eq!(
            effective_proxy_source(ProxyMode::System, with),
            ProxySource::Environment
        );
        assert_eq!(
            effective_proxy_source(ProxyMode::System, blank),
            ProxySource::System
        );
    }

    #[test]
    fn pipe_name_is_the_shared_ipc_helper() {
        let custom = Path::new(r"C:\Nope\Root");
        assert_eq!(pipe_name(None).unwrap(), pipe_name_for(None).unwrap());
        assert_eq!(
            pipe_name(Some(custom)).unwrap(),
            pipe_name_for(Some(custom)).unwrap()
        );
        let default = default_data_root().unwrap();
        assert_eq!(pipe_name(Some(&default)).unwrap(), pipe_name(None).unwrap());
    }
}
