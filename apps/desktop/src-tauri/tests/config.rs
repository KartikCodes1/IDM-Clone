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
        serde_json::json!(["allow-agent-status", "allow-list-downloads"]),
        "only the app commands, no core:* defaults"
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
