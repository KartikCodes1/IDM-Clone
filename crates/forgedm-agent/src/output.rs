//! The binary's designated console output: startup problems for a console build.
//!
//! The release binary uses the Windows subsystem and has no console, so this is only seen in
//! debug builds. Messages are error texts that never contain URLs or secrets.

/// Prints one error line to standard error.
#[allow(clippy::print_stderr)] // the single designated output point of the agent binary
pub fn error_line(message: &str) {
    eprintln!("forgedm-agent: {message}");
}
