//! Static checks of the Tauri configuration, capability and command manifest
//! (ADR-022, THREAT_MODEL Inv-23).

#![allow(clippy::expect_used)] // test code may expect (ARCHITECTURE 1.4)

use std::path::PathBuf;

use serde_json::Value;

fn crate_file(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_json(relative: &str) -> Value {
    let text = std::fs::read_to_string(crate_file(relative)).expect("file readable");
    serde_json::from_str(&text).expect("valid JSON")
}

const EXPECTED_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
img-src 'self'; font-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; \
base-uri 'none'; form-action 'none'; frame-src 'none'; frame-ancestors 'none'";

#[test]
fn config_grants_only_app_commands_and_strict_csp() {
    let config = read_json("tauri.conf.json");
    let security = &config["app"]["security"];

    assert_eq!(
        security["csp"], EXPECTED_CSP,
        "CSP must match ADR-022 exactly"
    );
    assert_eq!(security["freezePrototype"], true);
    assert_eq!(security["capabilities"], serde_json::json!(["main-window"]));
    assert_eq!(config["app"]["withGlobalTauri"], false);
    assert_eq!(config["identifier"], "dev.forgedm.desktop");
    assert_eq!(config["productName"], "ForgeDM");
    assert_eq!(config["app"]["windows"][0]["useHttpsScheme"], false);
    assert_eq!(config["bundle"]["targets"], serde_json::json!(["nsis"]));
    assert!(
        security
            .get("assetProtocol")
            .is_none_or(|a| a["enable"] != true),
        "the asset protocol must stay off"
    );

    let capability = read_json("capabilities/main-window.json");
    assert_eq!(capability["identifier"], "main-window");
    assert_eq!(capability["windows"], serde_json::json!(["main"]));
    assert_eq!(
        capability["permissions"],
        serde_json::Value::Array(
            forgedm_desktop_lib::COMMAND_NAMES
                .iter()
                .map(|name| serde_json::Value::String(format!("allow-{}", name.replace('_', "-"))))
                .collect()
        ),
        "exactly the app commands, each listed explicitly, no core:* defaults"
    );
    assert!(capability.get("remote").is_none(), "no remote origins");
    assert!(capability.get("webviews").is_none());
}

/// Command names in `build.rs`'s `AppManifest::commands(&[...])`.
fn manifest_commands() -> Vec<String> {
    let build = std::fs::read_to_string(crate_file("build.rs")).expect("build.rs readable");
    let start = build
        .find("commands(&[")
        .expect("AppManifest commands list")
        + "commands(&[".len();
    let end = start + build[start..].find("])").expect("list terminator");
    build[start..end]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn manifest_matches_registered_commands() {
    let registered: Vec<String> = forgedm_desktop_lib::COMMAND_NAMES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        manifest_commands(),
        registered,
        "build.rs manifest vs COMMAND_NAMES"
    );

    let lib = std::fs::read_to_string(crate_file("src/lib.rs")).expect("lib.rs readable");
    let handler_start = lib
        .find("generate_handler![")
        .expect("generate_handler! call");
    let handler_len = lib[handler_start..].find(']').expect("handler list end");
    let handler = &lib[handler_start..handler_start + handler_len];
    let in_handler: Vec<String> = handler
        .split("commands::")
        .skip(1)
        .map(|rest| {
            rest.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_owned()
        })
        .collect();
    assert_eq!(in_handler, registered, "generate_handler! vs COMMAND_NAMES");

    let capability = read_json("capabilities/main-window.json");
    let granted: Vec<String> = capability["permissions"]
        .as_array()
        .expect("permissions array")
        .iter()
        .map(|p| {
            p.as_str()
                .expect("string")
                .replace("allow-", "")
                .replace('-', "_")
        })
        .collect();
    assert_eq!(
        granted, registered,
        "capability grants exactly the registered commands"
    );
}

/// The full, reviewed command surface (W7, W10a). Kept as a literal so that adding a command
/// to `COMMAND_NAMES` without a review of this list fails.
const EXPECTED_COMMANDS: [&str; 18] = [
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
    "pause_all",
    "resume_all",
    "update_limits",
    "update_priority",
    "get_limits",
];

#[test]
fn command_surface_is_the_reviewed_list() {
    assert_eq!(forgedm_desktop_lib::COMMAND_NAMES, EXPECTED_COMMANDS);
    let capability = read_json("capabilities/main-window.json");
    let granted: Vec<&str> = capability["permissions"]
        .as_array()
        .expect("permissions array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(granted.len(), EXPECTED_COMMANDS.len());
    for name in EXPECTED_COMMANDS {
        let permission = format!("allow-{}", name.replace('_', "-"));
        assert!(
            granted.contains(&permission.as_str()),
            "{permission} granted"
        );
        let file = crate_file(&format!("permissions/autogenerated/{name}.toml"));
        assert!(file.is_file(), "permission file for {name} exists");
    }
}

#[test]
fn installer_overlay_is_per_user_nsis_with_both_sidecars_and_no_autostart() {
    let overlay = read_json("tauri.bundle.conf.json");
    let bundle = &overlay["bundle"];
    assert_eq!(
        bundle["externalBin"],
        serde_json::json!(["binaries/forgedm-agent", "binaries/forgedm"])
    );
    assert_eq!(bundle["targets"], serde_json::json!(["nsis"]));
    assert_eq!(bundle["windows"]["nsis"]["installMode"], "currentUser");
    assert!(
        overlay["build"]["beforeBuildCommand"]
            .as_str()
            .is_some_and(|c| c.contains("prepare-sidecars")),
        "the overlay builds and stages the sidecars before cargo runs"
    );
    // No autostart: neither a registry Run key nor a startup shortcut is configured here.
    let text = std::fs::read_to_string(crate_file("tauri.bundle.conf.json")).expect("readable");
    for forbidden in ["autostart", "Run\\", "Startup"] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    let base = read_json("tauri.conf.json");
    assert!(
        base["bundle"].get("externalBin").is_none(),
        "sidecars live only in the overlay so cargo test needs no prebuilt binaries"
    );
}

#[test]
fn desktop_manifest_requests_no_elevation_and_data_dir_is_outside_the_identifier() {
    let build = std::fs::read_to_string(crate_file("build.rs")).expect("build.rs readable");
    assert!(build.contains("asInvoker"), "explicit asInvoker manifest");
    assert!(build.contains("uiAccess=\"false\""));
    assert!(!build.contains("requireAdministrator"));
    assert!(!build.contains("highestAvailable"));
    let config = read_json("tauri.conf.json");
    // Tauri's uninstaller offers to delete only %LOCALAPPDATA%\<identifier>. The agent keeps
    // its data in %LOCALAPPDATA%\ForgeDM, which is never equal to the identifier folder.
    assert_ne!(config["identifier"], "ForgeDM");
}
