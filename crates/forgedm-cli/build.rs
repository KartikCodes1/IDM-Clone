//! Embeds an `asInvoker` application manifest into the binary (ForgeDM never asks for
//! elevation, `uiAccess` is off). Only the MSVC linker understands these arguments.

use std::io::Write;

fn main() {
    let msvc_windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if !msvc_windows {
        return;
    }
    let mut out = std::io::stdout();
    for arg in [
        "/MANIFEST:EMBED",
        "/MANIFESTUAC:level='asInvoker' uiAccess='false'",
    ] {
        // Cargo reads build-script directives from stdout; `println!` is denied by lint.
        let _ = writeln!(out, "cargo:rustc-link-arg-bins={arg}");
    }
    let _ = writeln!(out, "cargo:rerun-if-changed=build.rs");
}
