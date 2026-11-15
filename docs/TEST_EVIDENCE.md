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

### S0.3 Protocol v1 (2026-11-15)

RED: not recorded in the slice results (no RED run was reported).

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-protocol -- --check` | 0 | clean |
| `cargo clippy -p forgedm-protocol --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-protocol --locked` | 0 | 21 passed, 0 failed, 0 ignored: envelope 8, frame 6, golden 1, proptests 3, validate 1, version 2 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

An earlier run of `cargo test -p forgedm-protocol --locked` failed on a new test written in this slice: its input used a tolerant enum, so it parsed. The input was changed to a strict enum (`priority`); no implementation or other test was changed. The exit code of that earlier run was not recorded.

ecc:code-reviewer reported re-running the same test command with the same result (reviewer report, not a coordinator run).

NOT RUN (plan_tests_missing): none. All 12 plan tests exist under their exact names and pass. 5 tests were added beyond the plan.

### S0.4 Platform security primitives (2026-11-15)

RED: `cargo test -p forgedm-platform --locked --no-run` failed before implementation: `could not compile forgedm-platform (test "dpapi") due to 2 previous errors`, `(test "folders") due to 3`, `(test "security") due to 36`; e.g. E0425 cannot find function `create_pipe_instance` in module `pipe`, E0432 unresolved import `forgedm_platform::PlatformError`.

| Command | Exit | Result |
|---|---|---|
| `cargo test -p forgedm-platform --locked --test security` (first run, after the new tests were added) | 101 | not green: the new surrogate and pass-through tests and the SACL-flag assertion failed. Causes: test literals lost a backslash; the SACL expectation was wrong. |
| `cargo fmt -p forgedm-platform -- --check` | 0 | clean, after running `cargo fmt -p forgedm-platform` |
| `cargo clippy -p forgedm-platform --all-targets --locked -- -D warnings` | 0 | clean; no lint relaxed |
| `cargo test -p forgedm-platform --locked` | 0 | 23 passed, 0 failed, 0 ignored: security 14, folders 3, dpapi 6 (lib and doc-test binaries run 0 tests) |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

The surrogate test was written against the lossless `wide.rs` implementation and has no direct red run against the old lossy behaviour. That behaviour was reproduced earlier by the verifier, not in this slice's runs.

NOT RUN (plan_tests_missing): none. 8 tests were added beyond the plan minimum.
Not covered: no test shows that a low-integrity or foreign principal is denied, and `PIPE_REJECT_REMOTE_CLIENTS` is not asserted. Cross-user denial is verified only by the SDDL readback (open issue).

### S0.5 Fixture server baseline (2026-11-15)

RED: `cargo test -p forgedm-fixture --locked` before implementation: error[E0432] unresolved imports `forgedm_fixture::verify`, `forgedm_fixture::FixtureServer` and `forgedm_fixture::content` ("no content in the root"); `could not compile forgedm-fixture (test "content") due to 2 previous errors` and `(test "server") due to 1 previous error`.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-fixture -- --check` | 0 | clean |
| `cargo clippy -p forgedm-fixture --all-targets --locked -- -D warnings` | 0 | clean; only the 2 pre-existing root `clippy.toml` unreachable-path warnings |
| `cargo test -p forgedm-fixture --locked` | 0 | 22 passed, 0 failed, 0 ignored: unit 6, cli 2, content 3, server 11 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

ecc:rust-reviewer reported re-running the same test command with the same passing counts (reviewer report, not a coordinator run).

NOT RUN (plan_tests_missing): none. Not covered: the ctrl_c-failure fallback and the accept-error backoff have no regression tests (open issue).

### W1 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S0.3, S0.4 and S0.5; their RED lines are in the sections above.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt --all -- --check` | 0 | no formatting differences reported across the workspace |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | all 8 crates checked; no warnings or errors |
| `cargo test --workspace --locked` | 0 | 67 passed, 0 failed, 0 ignored across 30 result lines: core tls_smoke 1; fixture 22; platform 23; protocol 21; all other suites 0 tests |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

Run with cargo 1.100.0 and `CARGO_TARGET_DIR` unset. Gate all_green: true. Fixes applied during the gate: none. Full test log kept in the session scratchpad as `w1_test.log`.

NOT RUN (plan_tests_missing): none reported for the W1 slices.
