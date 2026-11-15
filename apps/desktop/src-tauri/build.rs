//! Generates the per-command permissions (`allow-agent-status`, ...) so that app commands
//! are ACL-checked; without an app manifest Tauri does not gate them (ADR-022).
//!
//! Also sets an explicit `asInvoker` application manifest: ForgeDM never requests elevation.

use std::io::Write;

/// Tauri's default manifest (Common Controls v6) plus an explicit `asInvoker` level.
const APP_MANIFEST: &str = r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&[
        "agent_status",
        "list_downloads",
        "get_snapshot",
        "get_download",
        "enqueue_url",
        "pause",
        "resume",
        "cancel",
        "retry",
        "remove",
        "start_engine",
        "stop_engine",
        "show_in_folder",
    ]);
    let windows = tauri_build::WindowsAttributes::new().app_manifest(APP_MANIFEST);
    let attributes = tauri_build::Attributes::new()
        .app_manifest(manifest)
        .windows_attributes(windows);
    if let Err(err) = tauri_build::try_build(attributes) {
        // build.rs has no logger; Cargo shows stderr when the build script fails.
        let _ = writeln!(std::io::stderr(), "tauri build script failed: {err:#}");
        std::process::exit(1);
    }
}
