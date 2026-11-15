//! Generates the per-command permissions (`allow-agent-status`, ...) so that app commands
//! are ACL-checked; without an app manifest Tauri does not gate them (ADR-022).

use std::io::Write;

fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&["agent_status", "list_downloads"]);
    let attributes = tauri_build::Attributes::new().app_manifest(manifest);
    if let Err(err) = tauri_build::try_build(attributes) {
        // build.rs has no logger; Cargo shows stderr when the build script fails.
        let _ = writeln!(std::io::stderr(), "tauri build script failed: {err:#}");
        std::process::exit(1);
    }
}
