//! Crash injection for the recovery tests (PLAN S1.14, Inv-24). Compiled only with the
//! `fault-injection` feature, which is never enabled in a release build.
//!
//! `FORGEDM_FAULT=<point>` names one of the points in [`forgedm_core::fault::FaultPoint`].
//! The first time the engine reaches it, the process ends at once with
//! [`FAULT_EXIT_CODE`], without unwinding, flushing or closing the database: what the next
//! start finds is what a power cut or `TerminateProcess` would leave.

use std::sync::atomic::{AtomicBool, Ordering};

use forgedm_core::fault::{self, FaultPoint};

/// The environment variable that selects the fault point.
pub const FAULT_ENV: &str = "FORGEDM_FAULT";
/// Exit code of a process that stopped at its fault point.
pub const FAULT_EXIT_CODE: i32 = 77;

/// Why the fault selection could not be applied.
#[derive(Debug, thiserror::Error)]
pub enum FaultError {
    /// `FORGEDM_FAULT` names no known point.
    #[error("{FAULT_ENV} names an unknown fault point")]
    UnknownPoint,
    /// A hook was installed earlier in this process.
    #[error("a fault hook is already installed")]
    AlreadyInstalled,
}

/// Reads `FORGEDM_FAULT` and, when it names a point, arms it. Returns the armed point.
///
/// # Errors
/// [`FaultError`] for an unknown name (the agent refuses to start rather than run without
/// the fault the test asked for).
pub fn install_from_env() -> Result<Option<FaultPoint>, FaultError> {
    let Ok(raw) = std::env::var(FAULT_ENV) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let point = FaultPoint::parse(&raw).ok_or(FaultError::UnknownPoint)?;
    arm(point)?;
    Ok(Some(point))
}

/// Arms `point`: its first hit ends the process.
///
/// # Errors
/// [`FaultError::AlreadyInstalled`] when a hook exists already.
pub fn arm(point: FaultPoint) -> Result<(), FaultError> {
    let fired = AtomicBool::new(false);
    let installed = fault::install(move |hit| {
        if hit == point && !fired.swap(true, Ordering::SeqCst) {
            tracing::error!(point = point.name(), "fault injected: ending the process");
            std::process::exit(FAULT_EXIT_CODE);
        }
    });
    if installed {
        Ok(())
    } else {
        Err(FaultError::AlreadyInstalled)
    }
}
