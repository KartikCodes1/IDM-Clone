//! Rolling file logs and the panic hook (ARCHITECTURE 8).
//!
//! JSON lines go to `<data-dir>\logs\agent.log.YYYY-MM-DD`, rotated daily with the newest 14
//! files kept, through tracing-appender's non-blocking writer. The returned [`LogGuard`] must
//! be held until the process is about to exit so buffered lines are flushed. Security events
//! use the target `forgedm::security`. Nothing here logs URLs; call sites must only pass
//! redacted URLs.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;

use crate::error::StartupError;

/// Prefix of the log files; the appender appends `.YYYY-MM-DD`.
const FILE_PREFIX: &str = "agent.log";
/// Rotated files kept (A.9).
pub const KEEP_FILES: usize = 14;

/// Keeps the background log writer alive; flushes buffered lines on drop.
#[must_use = "dropping the guard stops file logging"]
pub struct LogGuard {
    _worker: WorkerGuard,
}

/// Installs the global subscriber writing to `logs_dir`.
///
/// # Errors
/// [`StartupError::Logging`] when the directory is unusable, the filter is invalid or a
/// global subscriber already exists.
pub fn init(logs_dir: &Path, filter: &str) -> Result<LogGuard, StartupError> {
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .max_log_files(KEEP_FILES)
        .build(logs_dir)
        .map_err(|err| StartupError::Logging(err.to_string()))?;
    let (writer, worker) = tracing_appender::non_blocking(appender);
    let env_filter =
        EnvFilter::try_new(filter).map_err(|err| StartupError::Logging(err.to_string()))?;
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_env_filter(env_filter)
        .with_writer(writer)
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|err| StartupError::Logging(err.to_string()))?;
    Ok(LogGuard { _worker: worker })
}

/// Logs panics as `error` events. Only the location is logged: a payload may carry request
/// data. The default hook still runs so a console build shows the panic.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map_or_else(String::new, |l| format!("{}:{}", l.file(), l.line()));
        tracing::error!(%location, "the agent panicked");
        default(info);
    }));
}
