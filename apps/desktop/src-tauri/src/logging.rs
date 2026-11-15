//! Desktop log sink (ARCHITECTURE 8): a daily rolling file `desktop.<date>` under
//! `<data root>\logs`, 14 files kept, `RUST_LOG`-style filter defaulting to `info`.
//!
//! Nothing in the desktop crate logs URLs, file names or other user data; messages carry
//! error classes and local, non-user-supplied facts only. A logging failure never stops
//! the app: [`init`] falls back to a stderr sink and reports the reason exactly once.
//!
//! Lines are JSON objects, one per line, matching the agent's log format.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::Subscriber;
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{InitError, RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

/// File-name prefix of the rolling log (`desktop.2026-01-31`).
pub const LOG_FILE_PREFIX: &str = "desktop";
/// Number of daily files kept before the oldest is deleted.
pub const LOG_FILES_KEPT: usize = 14;
/// Folder under the data root that holds the logs.
pub const LOG_SUBDIR: &str = "logs";

/// Why the file sink could not be set up.
#[derive(Debug, Error)]
pub enum LoggingError {
    /// The default data root could not be resolved.
    #[error("the data folder could not be resolved: {0}")]
    DataRoot(String),
    /// The rolling appender could not be created.
    #[error("the log file could not be opened: {0}")]
    Sink(#[from] InitError),
    /// A global subscriber was already installed, so the file sink would go unused.
    #[error("a global log subscriber is already installed")]
    AlreadyInitialised,
}

/// Keeps the background log writer alive; dropping it flushes pending lines.
///
/// `tauri` ends the process with `std::process::exit`, which skips destructors, so the
/// app must drop this explicitly on the exit event (see `run`).
#[derive(Debug)]
pub struct LogGuard {
    _worker: Option<WorkerGuard>,
}

/// The log folder under `data_root`.
#[must_use]
pub fn log_dir_for(data_root: &Path) -> PathBuf {
    data_root.join(LOG_SUBDIR)
}

/// Builds the filter: `directives` (as in `RUST_LOG`) when given and valid, else `info`.
#[must_use]
pub fn default_filter(directives: Option<&str>) -> EnvFilter {
    let builder = EnvFilter::builder().with_default_directive(LevelFilter::INFO.into());
    builder.parse_lossy(directives.unwrap_or_default())
}

/// Opens the daily rolling file in `dir` (created if missing) behind a non-blocking writer.
///
/// # Errors
/// [`LoggingError::Sink`] when the folder or file cannot be created.
pub fn open_file_sink(dir: &Path) -> Result<(NonBlocking, WorkerGuard), LoggingError> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .max_log_files(LOG_FILES_KEPT)
        .build(dir)?;
    Ok(tracing_appender::non_blocking(appender))
}

/// A JSON-lines subscriber writing to `writer` through `filter`.
pub fn subscriber_with<W>(filter: EnvFilter, writer: W) -> impl Subscriber + Send + Sync
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .json()
        .finish()
}

/// Installs a JSON subscriber writing to `writer` as the process-wide default.
///
/// # Errors
/// [`LoggingError::AlreadyInitialised`] when a global subscriber already exists.
pub fn install_global<W>(filter: EnvFilter, writer: W) -> Result<(), LoggingError>
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    tracing::subscriber::set_global_default(subscriber_with(filter, writer))
        .map_err(|_| LoggingError::AlreadyInitialised)
}

fn file_guard(rust_log: Option<&str>) -> Result<LogGuard, LoggingError> {
    let root =
        forgedm_ipc::default_data_root().map_err(|err| LoggingError::DataRoot(err.to_string()))?;
    let (writer, worker) = open_file_sink(&log_dir_for(&root))?;
    install_global(default_filter(rust_log), writer)?;
    Ok(LogGuard {
        _worker: Some(worker),
    })
}

/// Installs the global subscriber. Never fails: when the file sink is unavailable it
/// installs a stderr sink and reports the reason once (directly on stderr if even that
/// cannot be installed).
#[must_use]
pub fn init() -> LogGuard {
    let rust_log = std::env::var("RUST_LOG").ok();
    match file_guard(rust_log.as_deref()) {
        Ok(guard) => guard,
        Err(err) => {
            if install_global(default_filter(rust_log.as_deref()), std::io::stderr).is_ok() {
                tracing::warn!(error = %err, "file logging unavailable; continuing without a log file");
            } else {
                // Last resort: no subscriber can be installed, so write straight to stderr.
                let _ = writeln!(std::io::stderr(), "file logging unavailable: {err}");
            }
            LogGuard { _worker: None }
        }
    }
}
