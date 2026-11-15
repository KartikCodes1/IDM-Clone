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

### S0.4b Platform pipe returns a tokio NamedPipeServer (PCR-04) (2026-11-15)

RED: added `pipe_instance_is_tokio_server_and_roundtrips_bytes` and moved the pipe tests to `#[tokio::test]`. The security test target failed to compile with E0308 (expected `NamedPipeServer`, found `OwnedHandle` at `create_pipe_instance`). The exit code of that compile was not reported. The change was then implemented and all tests passed.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-platform -- --check` | 0 | clean |
| `cargo clippy -p forgedm-platform --all-targets --locked -- -D warnings` | 0 | clean apart from the 2 pre-existing clippy.toml unreachable-path warnings. An earlier run failed on expect_used in the new helper; it was fixed with a local `#[allow(clippy::expect_used)]` |
| `cargo test -p forgedm-platform --locked` | 0 | 25 passed, 0 failed, 0 ignored: dpapi 6, folders 3, security 16 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

Deviations: the tokio dev-dependency features (net, rt, macros, time) do not include io-util, so the round-trip test uses try_read/try_write instead of AsyncReadExt/AsyncWriteExt. Interior-NUL names are still rejected by `to_wide_nul` before ServerOptions is used. The DACL assertions moved into a shared `assert_locked_down` helper. The security reviewer's LOW finding was fixed by adding `replacement_instances_carry_the_same_dacl_and_byte_mode`.

Open: PIPE_REJECT_REMOTE_CLIENTS is still not asserted, because neither tokio nor GetNamedPipeInfo exposes it and no remote client was available. Both reviewers read the code only and did not run cargo.

NOT RUN (plan_tests_missing): none reported.

### S0.6 Storage (2026-11-15)

RED: `cargo test -p forgedm-storage --locked --no-run` exited 101. The migrations, downloads and segments test targets failed to compile with E0432 (unresolved imports of Storage, NewDownload, SealContext, SealError, SecretSealer, NewEvent, ProbeResult, SegmentRecord, SettingKey, StorageError) and 5 E0425 errors (cannot find `sqlite_version_at_least`). The log path in the slice report was truncated, so it is not recorded here.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-storage -- --check` | 0 | clean |
| `cargo clippy -p forgedm-storage --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-storage --locked` | 0 | 25 passed, 0 failed, 0 ignored: lib 1, downloads 9, migrations 5, secrets 1, segments 9; doc-tests 0 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |
| Mutation check: begin_write changed to plain `begin()`, then `cargo test -p forgedm-storage --locked --lib`, then BEGIN IMMEDIATE restored | 101 | Expected failure: `db::tests::begin_write_takes_the_write_lock_up_front` FAILED against the deferred-BEGIN mutant. Code restored and verified (`begin_with("BEGIN IMMEDIATE")`) |

Deviations: test-only clippy allows are `#![allow(dead_code, clippy::unwrap_used)]` in tests/common/mod.rs and `#[allow(clippy::unwrap_used)]` on the `with_size` helper in tests/segments.rs. No production lint was relaxed. Four tests were added beyond the plan (seal_failure_inserts_nothing, replace_plan_and_load_roundtrip, settings_roundtrip_and_bounds, complete_set_finalize_target_and_delete), plus an in-crate db::tests unit test. All 18 plan test names exist and pass. Extra repository methods: save_probe, set_finalize_target, complete, delete, segment reset and load. SettingKey names were chosen by the slice (Appendix A does not define them). The test sealer uses std DefaultHasher; it is a test double only. transition does not validate edges against A.6; that belongs to core (S1.1).

Open: plaintext URL zeroing (DB-04, ADR-012, ARCHITECTURE 7.5) is unmet; it needs zeroize (a new dependency, not allowed) or a hand-rolled wipe. tracing and tokio remain normal dependencies because Cargo.lock is frozen. Cross-process SQLITE_BUSY contention is untested beyond the single-holder unit test. Write-time tiling validation was not added (load-time by design).

NOT RUN (plan_tests_missing): none reported.

### S0.7 IPC transport (2026-11-15)

RED: not recorded in the slice results (red field empty). The deviations note that `seventeenth_client_waits_then_served` failed before the bounded ERROR_PIPE_BUSY retry was added and passed after it; no exit code was reported for that run.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-ipc -- --check` | 0 | clean |
| `cargo clippy -p forgedm-ipc --all-targets --locked -- -D warnings` | 0 | clean. An earlier run failed on an unused test helper (exit not recorded); the helper was removed |
| `cargo test -p forgedm-ipc --locked` | 0 | 28 passed, 0 failed, 0 ignored: transport 18, identity 7, subscribe 3, lib 0 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

The rust-reviewer reported re-running the same test command with the same counts (reviewer report, not a coordinator run).

Deviations: bounded retry of replacement-instance creation on ERROR_PIPE_BUSY only (at most 1 s, 5 ms steps); any other error, or a persistent busy, is fatal. The plan says creation is never retried without first_pipe_instance(true); the security reviewer read the retry as consistent with Inv-14, and the coordinator must confirm. A stuck-reader write timeout (`Timeouts.write`, default 10 s) is not in Appendix A.9. The frame timeout is measured from the first byte of a frame. `serve_connection` and `Client::from_verified_stream` are public doc(hidden) test seams; the latter does no identity check by design. Test files carry file-level `#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]`, because clippy.toml does not cover integration-test helpers. `low_integrity_client_denied` now panics when its setup fails instead of printing SKIPPED. Added IpcError::UnexpectedResult and IpcError::InvalidPipeName, and the regression test `client_rejects_non_local_pipe_names`.

Open: S0.8 must bound `Server::serve` with a timeout, because handlers are not cancellable. The coordinator may add WRITE_TIMEOUT to Appendix A.9 and amend the S0.7 and ADR-005 wording on the ERROR_PIPE_BUSY retry. `pipe_name_never_vanishes_under_churn` counts IpcError::Busy as a soft outcome and asserts at least 100 handshakes served.

NOT RUN (plan_tests_missing): none reported.

### S1.1 URL policy, naming, state machine (2026-11-15)

RED: not recorded in the slice results (red field empty).

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-core -- --check` | 0 | no formatting differences (after `cargo fmt -p forgedm-core`) |
| `cargo clippy -p forgedm-core --all-targets --locked -- -D warnings` | 0 | clean. An earlier run failed on a dead-code warning from an edit that had not applied; fixed, and no lint relaxed |
| `cargo test -p forgedm-core --locked` | 0 | 40 passed, 0 failed, 0 ignored for this slice's binaries: naming 20, state_machine 8, tls_smoke 1, url_policy 11 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

The S1.2 run of the same command reported 97 for the whole crate, which includes the S1.2 binaries.

Deviations: lib.rs was not touched; the tests reach the API through existing `pub mod` paths. Resume and Retry are no-ops (changed=false) in queued, probing, downloading, pausing, verifying and finalizing. A.6 does not say whether pausing, verifying or finalizing count as active, so this is an interpretation. Pause is a no-op in paused and pausing; Cancel is a no-op in cancelled; everything else not listed returns InvalidState. Extra public API: state::Command, state::Transition, state::TransitionError, state::part_file_invalid_outcome, url_policy RedactedUrl, HostClass::is_local, and naming helpers. NAT64 is classified by its embedded IPv4 address.

Open: reserved IPv4 ranges (224/4, 240/4, 255.255.255.255, 192.0.0.0/24, 198.18/15) and IPv6 multicast still classify as Public; they are left to the later DNS-resolved policy (EXT-01). ARCHITECTURE 4.14 step 3 needs a docs update for the widened removal set.

NOT RUN (plan_tests_missing): none reported.

### S1.2 HTTP range semantics and redirect policy (2026-11-15)

RED: not recorded in the slice results (red field empty).

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-core -- --check` | 0 | clean |
| `cargo clippy -p forgedm-core --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-core --locked` | 0 | 97 passed, 0 failed, 0 ignored: content_range 9, naming 20, probe_classify 8, range_decision 24, redirect 10, state_machine 8, tls_smoke 1, url_policy 11, validators 6 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

The rust-reviewer and code-reviewer re-runs reported range_decision 23 (56 for the five S1.2 targets), one fewer than the coordinator run. The fix list records one test added for rust-reviewer-1, which may account for the difference; this was not verified.

Deviations: seven plan test names begin with a digit and carry a leading underscore; a substring filter such as `cargo test 416_no_cr_reprobe` still matches. decide() takes a RangedRequest (range, if_range_sent, segment_no, has_committed_data). ResponseMeta holds raw header strings, parsed strictly inside the crate. Core's existing forgedm_protocol enums are reused; no new dependency. Design choices where ARCHITECTURE 4.6 is silent: validator-less responses, exact total match for 206 and 416, `*` accepted only while the stored total is unknown, redirect Location with userinfo or control characters blocked, and no fragment on a resolved redirect. One test input changed from `http:///x` to `http://`, because the WHATWG parser accepts `http:///x` with host `x`. Test helpers use unreachable!() because clippy.toml allows unwrap and expect in tests but not panic.

Open: a 206 whose stored total is unknown but whose Content-Range reports a total is a TotalMismatch (strict). Relaxing it needs ARCHITECTURE 4.6 and `rejects_206_total_mismatch` changed together, by the owner.

NOT RUN (plan_tests_missing): none reported.

### W2 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S0.4b, S0.6, S0.7, S1.1 and S1.2; their RED lines are in the sections above.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt --all -- --check` (CARGO_TARGET_DIR unset, cargo 1.100.0) | 0 | no formatting differences across the workspace |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | all workspace crates checked (protocol, platform, storage, core, ipc, cli, agent, fixture); no warnings or errors |
| `cargo test --workspace --locked` | 0 | 218 passed, 0 failed, 0 ignored across 45 result lines (38 test binaries, 7 doc-test runs): core 97; fixture 22; ipc 28 (identity 7, subscribe 3, transport 18); platform 25 (dpapi 6, folders 3, security 16); protocol 21; storage 25 (lib 1, downloads 9, migrations 5, secrets 1, segments 9). No warning lines in the test output |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

Gate all_green: true. Fixes applied during the gate: none. Problems: none.

NOT RUN (plan_tests_missing): none reported. Not in the W2 gate command list, so not run: `cargo deny --locked check` and coverage (cargo-llvm-cov). The 80% coverage target is therefore not verified for this wave.
