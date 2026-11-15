# Test Evidence

Every entry here is a command that was actually run: its exit code, the counts it reported, and the date. Checks that were not run are recorded as **NOT RUN** with the reason. A proposed test is never recorded as a passing one.

## Environment (2026-11-15)

| Item | Value | How verified |
|---|---|---|
| OS | Windows 11 Pro 10.0.26300 x64 | inspection |
| Rust | rustc 1.100.0, cargo 1.100.0, clippy 0.1.100, rustfmt 1.11.0 (stable-x86_64-pc-windows-msvc) | `rustc --version` etc., exit 0 |
| MSVC linker | VS 2022 Build Tools (`vswhere`) | scratch `cargo new` + `cargo build` exit 0, binary ran |
| Rust (pinned) | toolchain `1.100.0-x86_64-pc-windows-msvc` (minimal + rustfmt, clippy, llvm-tools-preview) | `rustup run 1.100.0 rustc --version` exit 0 |
| Coverage / audit tools | cargo-llvm-cov 0.9.1, cargo-deny 0.20.2 (`cargo install --locked`); cargo-audit not installed | `--version`, exit 0 |
| Node / pnpm | Node 26.13.1 LTS installed via the user's nvm-windows 1.2.2 at `%LOCALAPPDATA%\nvm\v26.13.1` (global active Node left at 25.5.0; winget `--scope user` had no installer: MSI is machine-scope only); pnpm 10.29.2 | `v26.13.1\node.exe --version` = v26.13.1 |

## Slice evidence

### S0.1 Workspace scaffold (2026-11-15)

RED: in a minimal core-only workspace, `cargo test -p forgedm-core --test tls_smoke` exited 101 with `E0433 cannot find module or crate reqwest` (tls_smoke.rs:7). This was before the dependency set was added.

| Command (repo root) | Exit | Result | Run by |
|---|---|---|---|
| `cargo generate-lockfile` | 0 | 326 packages locked | implementer |
| `rustc --version` | 0 | rustc 1.100.0 (via the `rust-toolchain.toml` pin) | implementer, coordinator re-run |
| `cargo metadata --locked --no-deps` | 0 | 8 packages: protocol, platform, core, fixture, storage, ipc, agent, cli | implementer, coordinator re-run |
| `cargo build --workspace --locked` | 0 | aws-lc-sys 0.47.0 (prebuilt NASM objects) and libsqlite3-sys 0.37.0 built from the spaced path | implementer, fixer |
| `cargo test -p forgedm-core --test tls_smoke --locked` | 0 | 1 passed (`client_builds_with_configured_tls_provider`) | implementer, fixer |
| `cargo test --workspace --locked` | 0 | 1 passed, 0 failed (17 empty test binaries) | implementer, coordinator re-run |
| `cargo fmt --all --check` | 0 | clean | implementer, fixer, coordinator re-run |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | clean; all disallowed-method paths resolve except `tls_early_data` (`allow-invalid`, gated on the unused `http3` feature) | implementer, fixer, coordinator re-run |
| clippy negative check: a temporary test calls `reqwest::Client::new` and `std::fs::rename` | 101 | both flagged as disallowed, so the guard is active; the temp file was removed | implementer |
| `cargo tree -e features -i reqwest --locked` | 0 | features `rustls`, `__rustls`, `__rustls-aws-lc-rs`, `__tls`, `system-proxy` only; no gzip/brotli/deflate/zstd/http2 | implementer, coordinator re-run |

Key resolved versions: tokio 1.54.1, reqwest 0.13.5, rustls 0.23.46, aws-lc-rs 1.19.0, sqlx 0.9.0, libsqlite3-sys 0.37.0, serde 1.0.229, serde_json 1.0.154, clap 4.6.7, sha2 0.11.1 (and 0.10.9 transitive via sqlx), uuid 1.29.2, url 2.5.8, windows 0.62.2.

Not run in S0.1: `cargo deny --locked check` (scheduled for the Phase 0/1 gates) and coverage (no logic exists yet).
