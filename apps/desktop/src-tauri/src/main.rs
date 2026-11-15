//! `forgedm-desktop.exe`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    let log = forgedm_desktop_lib::logging::init();
    match forgedm_desktop_lib::run(log) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "the desktop shell exited with an error");
            ExitCode::FAILURE
        }
    }
}
