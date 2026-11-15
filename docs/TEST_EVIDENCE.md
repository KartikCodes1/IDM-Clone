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

### S0.8 Agent skeleton (2026-11-15)

RED: not recorded in the slice results (red field empty).

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-agent -- --check` | 0 | clean |
| `cargo clippy -p forgedm-agent --all-targets --locked -- -D warnings` | 0 | clean. An earlier run failed with exit 101 after dependencies were removed, which needed a Cargo.lock change; that change was reverted |
| `cargo test -p forgedm-agent --locked` | 0 | 68 passed, 0 failed, 0 ignored: 53 unit, 2 hub (tests/hub.rs), 13 process (tests/process.rs) |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

NOT RUN (plan_tests_missing): none reported. Not run by the slice: `cargo build --release` (windows_subsystem = windows); only the debug profile was built and tested.

Deviations: the two plan tests in tests/hub.rs (no_event_lost_between_snapshot_and_subscribe and subscribe_after_evicted_seq_requires_resync) run in-process and change state through the public HubHandle::publish. The other 11 plan tests run the real forgedm-agent.exe through CARGO_BIN_EXE_forgedm-agent. No test-only hook was added to the binary. The argument parser is hand-written instead of clap, because clap would change Cargo.lock. StateHub keeps every stored download (loaded up to the 10,000 queue cap); ListDownloads.total is exact, and GetSnapshot returns only the newest 1000 and sets truncated. A Subscribe whose replay would exceed the 256-frame outbound queue returns resync_required with reason lagged, and the subscriber is not registered. AgentStatus events are emitted only on lifecycle changes (starting to ready, then shutting_down); the 1 s periodic status belongs to S1.7. ProxySource::Environment is reported when --proxy system and any of HTTP_PROXY, HTTPS_PROXY or ALL_PROXY (either case) is non-empty; otherwise system. The `[features] fault-injection = []` declaration present at HEAD was removed from crates/forgedm-agent/Cargo.toml; S1.14 must re-add it, because PHASE_0_1_PLAN, DEVELOPMENT.md and the check.ps1 no-fault-injection-in-release check (Inv-24) rely on it. Extra tests beyond the plan list include bad_arguments_exit_2 and data_dir_with_reparse_point_exits_6 (it ran, it was not skipped), plus unit tests for config, data_dir, error mapping, sealer, reconcile, summary, hub and handler (including handler_timeout_maps_to_busy with paused time). The two LOW regression tests (set_state_to_the_current_state_is_a_no_op and closed_subscribers_are_pruned_on_the_next_record) are unit tests in events_hub.rs; no separate RED run was captured for them.

Open: anyhow and serde are unused dependencies that could not be removed without a Cargo.lock change, so the lock owner must act. The DACL check on an explicit --data-dir and the log-filter scoping are not fixed. Failures before logging starts (exit codes 2, 3, 6) leave no diagnostic in release builds; only a comment in main.rs documents this.

### S1.4 Fixture adversarial scenarios (2026-11-15)

RED: scenarios test binary before implementation: `test result: FAILED. 2 passed; 16 failed` (bad_cr_variants_emitted, short206_*, stall_after_*, mutable_etag_*, lm_eq_date_*, status_codes_*, fail_first_*, max_conns_*, multipart_*, range_200_after_*, slow_from_*, redirect_* x4 and auth_basic_* all FAILED). Raw test binary: compile error E0599 `no method named max_concurrency found for struct RequestLog`.

| Command | Exit | Result |
|---|---|---|
| scenarios test binary with `--test-threads=4`, working set polled by tasklist every 0.2 s, killed if over 300 MB | 0 | 21 passed, 0 failed, 1.0 s. Peak working set 7972 KB (about 8 MB). The reported 6.9 GB growth could not be reproduced on the current tree |
| `cargo test -p forgedm-fixture --locked --test memory -- --nocapture` | 0 | 1 passed. The first attempt failed with "cannot update the lock file", apparently transient from another slice's Cargo.toml edit mid-run; a retry about 20 s later passed. Cargo.lock unchanged |
| `cargo fmt -p forgedm-fixture -- --check` | 0 | clean |
| `cargo clippy -p forgedm-fixture --all-targets --locked -- -D warnings` | 0 | no fixture warnings. The 2 pre-existing unreachable-path warnings from the root clippy.toml (tokio::fs entries) are still reported; that file is outside the slice |
| `cargo test -p forgedm-fixture --locked` | 0 | 63 passed, 0 failed, 0 ignored: 12 unit, 2 cli, 3 content, 1 memory, 13 raw, 21 scenarios, 11 server |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

NOT RUN (plan_tests_missing): optional TLS mode (D9) is not implemented and is deferred to S1.12. Adding rcgen or tokio-rustls to forgedm-fixture would change Cargo.lock, which this wave forbids. The `downgrade` redirect is only a Location string until TLS exists.

Deviations: raw flag spellings chosen where the plan was open are `raw=dup_cl|cl_and_te|bad_status|huge_headers|crlf_location`. `lie_cl` is also served on the raw socket path, so it only applies to the first request on a connection; a raw or lie_cl flag on a reused keep-alive connection gets 400. Raw detection peeks at the first request line of each new connection, and raw responses always close the connection. Concurrency means in-flight requests: a slot is held until the response body is finished or dropped. `max_conns=N` returns 503 when N slots are held, and that 503 neither takes a slot nor raises the high-water mark. `short206=N` returns a consistent but shorter 206 (Content-Range and Content-Length both cover N fewer bytes, keeping at least 1 byte). `slow_from` is an absolute content offset; `stall_after` and `drop_after` count from the start of the served body. `slow_from` needs `slow`, `retry_after` and `fail_first` need `status`, and `mutate_after` needs `etag=mutable`; otherwise the request gets 400. With `etag=mutable&mutate_after=K` the count covers every request to the same `t` key and path; from request K the seed and the ETag change. `multipart=1` answers any valid range request (including a single range) as multipart/byteranges, built in memory, so bodies over 8 MiB fall back to normal behaviour. `redirect=signed_expiring` issues a new `signed=sN` token on each redirect; a token works `ttl_requests` times (default 1), then returns 403. The `status` flag accepts only 401, 403, 404, 410, 416, 429, 500 and 503, and Retry-After is sent verbatim on the forced response. The regression test uses a separate fixture process sampled with tasklist, not a counting global allocator, because the workspace lints forbid unsafe. There is no RED run for the memory test: making the overflow guard fail would allocate gigabytes, which the 300 MB resource rule forbids. The test asserts that the sampler read a non-zero working set, so it cannot pass vacuously. The interrupted run had already applied the fixes for review findings 1, 2, 4, 5, 6 and 7 and the loosened timing in 3; these were verified in the current tree, and only the log cap, the multipart part cap and the memory test were added.

Open: the 6.9 GB growth was not reproduced. The current code streams every body in 64 KiB chunks, and the multipart sum is a saturating fold checked against an 8 MiB cap before anything is assembled. The most likely cause was the overlapping-range sum overflowing in the half-edited state; this is an inference from reading the code, not an observation. Two hardening limits were added: the request log is capped at 50,000 entries (oldest dropped, seq keeps counting), and multipart is capped at 256 parts, falling back to the plain response. The 2 root clippy.toml warnings remain unresolved.

### S1.3 Retry policy, token bucket, estimator (2026-11-15)

RED: `cargo test -p forgedm-core --locked` exited 101 when run with the new progress test, before the progress.rs fix landed: regressed_counter_does_not_double_count_recovered_bytes failed (72.29 > 0). This is the RED for review finding 3. The first edit script missed progress.rs (CRLF line endings), so only the retry.rs clamp had applied at that point. No RED run was recorded for finding 1 (the on_throttled panic); the reviewer's verifier reproduced that panic with a standalone rustc program.

| Command | Exit | Result |
|---|---|---|
| `cargo test -p forgedm-core --locked` (with the new progress test, before the progress.rs fix landed) | 101 | RED, as above |
| `cargo fmt -p forgedm-core -- --check` | 0 | clean |
| `cargo clippy -p forgedm-core --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-core --locked` | 0 | 127 passed, 0 failed, 0 ignored across the crate; progress 10, retry 12 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

NOT RUN (plan_tests_missing): none reported.

Deviations: test `stall_decays_to_zero` was corrected; the implementation was not changed for this. The test first asserted eta()==None after a 60 s stall, which was wrong: the ETA speed is double-smoothed (tau 12 s) and is still about 42 KiB/s then, so the ETA is legitimately still shown under the 1 KiB/s gate. The test now checks that display speed is below 1 B/s at 60 s, then that ETA speed is below 1 KiB/s and eta()==None after a further 180 s of stall. Disposition::WaitUntil{at} uses tokio::time::Instant (monotonic), so a caller that needs retry_at_ms must convert. parse_retry_after takes response_date: Option<SystemTime>; an HTTP-date Retry-After with no response Date returns None (ignored), and a caller may pass local now as a fallback; an overflowing delta-seconds saturates to Duration::MAX, which classifies as Fatal(RateLimited). The budget counts the failure that reaches the limit as exhausting it: 8 consecutive failures per segment and 20 per download, with the first 7 and 19 returning Ok(attempt). TransferErrorKind::MalformedFraming maps to Fatal(range_protocol_violation), since Appendix A.7 has no closer code. TokenBucket takes an optional parent (with_parent) for hierarchical acquire instead of a free function; pieces are at most 64 KiB and are charged to the child, then the parent; set_rate wakes waiters through a Notify. Extra public API with tests: Ewma<N>, SpeedMeter (per-worker tau 2.5 s), ReresolveLimiter (3 per hour), sleep_or_cancel, BoxFuture, TokioClock, TokioSleeper and burst_for. Extra tests: transfer_error_classification_table, reresolve_limited_to_three_per_hour, burst_is_max_of_250ms_and_64kib, tick_is_250ms, worker_speed_tau_2_5s, eta_uses_min_of_speed_and_rate_limit, zero_dt_and_regressing_counters_are_safe, and a unit test in clock.rs.

Open: S1.3-rust-reviewer-2 (LOW) was left as is. on_refused_at_open does not set a cooldown, which differs from the ARCHITECTURE 4.11 wording; either the docs or the code should be aligned in a follow-up.

### S0.10 Desktop minimal shell (2026-11-15)

RED: Rust: `cargo test -p forgedm-desktop --locked` failed to compile with `error[E0433]: cannot find type CommandErrorKind in this scope` (apps\desktop\src-tauri\src\error.rs:9 and 16-20), 7 errors, exit 101. TS: `vitest run` failed with `Failed to resolve import ./ipc from src/lib/ipc.test.ts`; Test Files 3 failed (3), no tests run. The slice's RED note also mentions a test-side bug found during GREEN, but the note is cut off after the words "heredoc strip", so that detail is not recorded here. The test-side fix that was made is listed in the deviations below.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-desktop -- --check` | 0 | clean |
| `cargo clippy -p forgedm-desktop --all-targets --locked -- -D warnings` | 0 | no warnings |
| `cargo test -p forgedm-desktop --locked` | 0 | 6 unit + 3 + 2 integration passed, 0 failed (11 Rust tests) |
| `pnpm exec vitest run` | 0 | 3 files, 14 tests passed |
| `pnpm exec tsc --noEmit` | 0 | clean |
| `pnpm exec eslint .` | 0 | no output (clean) |
| `git diff --exit-code Cargo.lock` | 1 | Non-zero because Cargo.lock still carries the uncommitted tauri and tauri-build additions from the original S0.10 round (and other slices' edits). This round did not touch Cargo.lock; `cargo --locked` succeeded, so it was not modified |

NOT RUN (plan_tests_missing): none reported.

Deviations: forgedm-desktop may depend only on forgedm-ipc and forgedm-protocol, so it cannot call platform's Sid API. The current user's SID is read from System32\whoami.exe /user (CREATE_NO_WINDOW, SID strictly validated). Trust is unaffected, because Client::connect verifies the server's SID and image path. Recommended: ipc (S1.8 bootstrap) exports a default_pipe_name() so the desktop can drop the subprocess. Extra tests beyond the plan list: command-error mapping without leaking detail, whoami parsing, sibling agent path, non-local pipe rejection, list_downloads with an absent pipe, known_size percentage, malformed_payload error, unknown_state label, format tests, and parseCommandError. Test fix: in malicious_name_renders_as_text, querySelector('img') was wrong because the brand mark is an img; it now asserts no img[src='x'] and exactly one img. The injection assertions were not weakened. tauri.conf.json sets build.devUrl (http://localhost:5173), so plain cargo build, clippy and test compile without a built dist/ (verified); tauri build uses frontendDist and embeds dist (verified). beforeDevCommand is unset. Tauri default features are kept (wry, compression, common-controls-v6) and no plugins are used. The workspace features array is empty, no onlyBuiltDependencies exceptions were needed, and no .npmrc was needed. Polling is every 2 s in Phase 0; the plan's 1 Hz polling is Phase 1. Cargo.lock came from one non-locked cargo fetch after the root manifest entries were added: 251 additions and 0 existing (name, version) pairs changed. Cargo re-rendered the dependency lists of existing packages (for example "thiserror" became "thiserror 2.0.21"), which inflates the textual diff. The LOW fixes have no separate RED run; the new Rust test locator_failure_is_not_cached_and_success_is was written together with its fix and was not seen failing against the old code.

Open: tracing events go nowhere (MEDIUM, open): the desktop crate has no tracing subscriber or log sink. Installing one needs tracing-subscriber or tracing-appender as a forgedm-desktop dependency, which changes Cargo.lock and breaks the no-new-dependency rule, so a coordinator dependency request is needed. The comments in error.rs now say the events are currently discarded. Startup errors in main.rs are still dropped for the same reason. whoami.exe still runs synchronously and its path is built from SystemRoot; it now runs lazily on the first command rather than at startup. The proper fix is the token SID via forgedm-platform.

### W3 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S0.8, S1.4, S1.3 and S0.10; their RED lines are in the sections above (none is recorded for S0.8).

| Command | Exit | Result |
|---|---|---|
| `unset CARGO_TARGET_DIR; cargo fmt --all -- --check` | 0 | no formatting differences in the workspace; cargo and rustc 1.100.0, shared default target dir |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | clean for all crates, including forgedm-desktop (apps/desktop/src-tauri), forgedm-agent, forgedm-core and forgedm-fixture; finished in 1m 18s |
| `cargo test --workspace --locked` | 0 | 368 passed, 0 failed, 0 ignored across 58 test-result blocks (unit, integration and doc-tests). Per crate: agent 68 (53 lib, 2 hub, 13 process); core 127; desktop 11 (6 lib, 3, 2); fixture 63; ipc 28; platform 25; protocol 21; storage 25. No compiler warnings in the log |
| background memory monitor during `cargo test` (500 ms Get-Process sampling of processes under target\, auto-kill above 1 GB) | 0 | No process went over 1 GB, so nothing was killed. Highest sampled working set 22.1 MB (hub test binary); forgedm-agent children peaked at 18.8 MB. 10 processes sampled; shorter-lived test binaries finished between samples. No leftover target processes |
| `git diff --exit-code Cargo.lock` | 1 | Non-blocking, see problems below. 251 packages added; 0 existing (name, version) pairs changed or removed (334 at HEAD, 585 now). Diff +3027/-115 lines, mostly Cargo's version-qualified renames in dependency lists. Lock sha256 b547cb40798f5908c34c1191c86d3814b0a939a9371ed2fc135385f0421ced39, unchanged across the test and tauri build steps |
| `node --version` (PATH set to the nvm v26.13.1 directory first) | 0 | v26.13.1; `pnpm --version` gives 10.29.2 |
| `pnpm install --frozen-lockfile` | 0 | lockfile up to date, resolution skipped; 2 workspace projects already up to date |
| `pnpm -r lint` | 0 | eslint in @forgedm/desktop: no errors or warnings |
| `pnpm -r typecheck` | 0 | tsc --noEmit in @forgedm/desktop: clean |
| `pnpm -r test` | 0 | vitest 5.1.0: 3 test files, 14 tests passed, 0 failed |
| `pnpm audit --prod` | 0 | No known vulnerabilities found |
| `pnpm --filter @forgedm/desktop tauri build --debug --no-bundle` | 0 | beforeBuildCommand `pnpm run build` (tsc --noEmit and vite build) succeeded; JS bundle 227.26 kB. Cargo dev profile finished in 53.39s and reported the built application at target\debug\forgedm-desktop.exe. No warnings in the log |
| `test -f target/debug/forgedm-desktop.exe` | 0 | exists: 11,763,200 bytes, mtime 2026-11-15 23:21:01 (rebuilt by the tauri build) |

Gate all_green: true. Workspace tests: 368 passed, 0 failed, 0 ignored. Fixes applied during the gate: none.

Problems (all non-blocking):
- `git diff --exit-code Cargo.lock` exits 1 against HEAD because of S0.10's permitted tauri and tauri-build additions. all_green is true only because that delta is the permitted one; a reader who treats any non-zero exit as failure should read this step as passing for that reason.
- PATH: the global pnpm shim (C:\nvm4w\nodejs\pnpm.cmd, which runs Node v25.5.0) comes before Node 26 on PATH, and tauri's beforeBuildCommand starts a child cmd process that cannot see a bash function. Session-local wrappers that run v26.13.1 with the same pnpm.cjs were placed first on PATH in the session scratchpad; `where pnpm` and `pnpm exec node --version` confirmed v26.13.1. Nothing was installed and no global setting was changed. A later wave that runs tauri build with only the bash function would run pnpm under Node 25.
- `tauri build` calls cargo without --locked. The Cargo.lock hash was unchanged afterwards.
- S0.8 removed the `fault-injection` feature declaration from crates/forgedm-agent/Cargo.toml (present at HEAD). PHASE_0_1_PLAN S1.14, DEVELOPMENT.md and the check.ps1 no-fault-injection-in-release check (Inv-24) rely on it, and S1.14 must re-add it. This does not affect W3 gate results.
- The memory monitor sampled every 500 ms, so only 10 target processes were caught; the rest exited within half a second. Every sampled peak was at or under 22.1 MB.

NOT RUN: not in the W3 gate command list, so not run: `cargo deny --locked check` and coverage (cargo-llvm-cov). The 80% coverage target is therefore not verified for this wave. Plan item not run: S1.4 optional TLS mode (D9), deferred to S1.12.

### S1.5 Platform filesystem primitives (2026-11-15)

RED: `cargo test -p forgedm-platform --locked --test fs --no-run` failed with `error[E0432]: unresolved imports forgedm_platform::fs::IoClass, allocated_size, classify_io, create_part_file, delete_by_handle, ensure_within, file_id, final_path, finalize_move, free_space, is_name_surrogate, move_no_replace, reparse_tag_is_name_surrogate, set_sparse, volume_info` and `could not compile forgedm-platform (test "fs"`. The exit code was not recorded, and the recorded text ends there. Review-fix tests were added with their fixes and were not run red first (all findings are LOW).

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-platform -- --check` | 0 | clean |
| `cargo clippy -p forgedm-platform --all-targets --locked -- -D warnings` | 0 | clean; the 2 pre-existing clippy.toml path warnings remain. An earlier run (exit not recorded) failed on a panic in a test, which was fixed |
| `cargo test -p forgedm-platform --locked` | 0 | 50 passed, 0 failed, 0 ignored: lib 1, dpapi 6, folders 3, fs 24, security 16 |
| `git diff --exit-code Cargo.lock` | 1 | Non-zero from a Cargo.lock diff already present at session start, not from this slice. Its md5 was identical across this slice's runs. All cargo commands used `--locked` |

Deviations: `finalize_move` takes `on_candidate` as a fallible `FnMut(&Path) -> Result<(), PlatformError>`, so a failed persist aborts the move; the sleeper is an `FnMut(Duration)` closure, not a trait. `on_candidate` runs once per candidate, before its first attempt. The retry budget is the sum of requested sleeps (100 ms doubling, 2 s cap, shared across candidates), so tests never sleep. Budget exhaustion is reported as the Win32 code for 5 and as SharingViolation for 32 (see SECURITY_REVIEW S1.5). Added beyond the list: `create_part_file`, `open_part_file`, `delete_by_handle`, `FileId`, `VolumeInfo`, `IoClass`, `classify_code`, `reparse_tag_is_name_surrogate`; `finalize_move` rejects empty names and names containing separators, ':' or NUL. The name limit is 233 UTF-16 units (Appendix A.9 cap, leaving room for the ' (999)' suffix). Five extra tests. `tests/fs.rs` has a file-level `allow(clippy::expect_used, clippy::print_stderr)`; workspace lints are unchanged. `error.rs`, `lib.rs` and `Cargo.toml` were not touched.

Open: no integration test for an ACL-denied destination directory (needs icacls setup). The mapping is covered by the unit test `exhausted_access_denied_keeps_its_win32_code` and the sharing path by `retry_budget_exhausts`. The Cargo.lock diff predates this slice.

NOT RUN (plan_tests_missing): none.

### S1.6 Mark-of-the-Web (2026-11-15)

RED: not recorded in the slice results (red field empty). The worker-skip regression test was shown failing by a temporary mutation (row 2 below), not by a red run before the fix.

| Command | Exit | Result |
|---|---|---|
| `cargo test -p forgedm-platform --locked --test motw -- --include-ignored` (after fixes) | 0 | 21 passed, 0 failed, 0 ignored; the real-IAE test now asserts `Marked` and passed |
| Mutation: flag-clear in the worker skip branch removed, then `cargo test --test motw skipping`, then restored | 101 | Regression test failed with "stuck poisoned in trial 37", so it catches the bug. The first version (100 trials, sleep-based offsets) did not catch it and was replaced with a spin-based sweep of 400 trials |
| `cargo fmt -p forgedm-platform -- --check` | 0 | clean |
| `cargo clippy -p forgedm-platform --all-targets --locked -- -D warnings` | 0 | clean; only the 2 pre-existing clippy.toml warnings |
| `cargo test -p forgedm-platform --locked` (3 runs) | 0 | Each run: lib 1, dpapi 6, folders 3, fs 24, motw 20 (+1 ignored), security 16; 0 failed in every run; stable across the 3 runs. Per run: 70 passed, 0 failed, 1 ignored |
| `git diff --exit-code Cargo.lock` | 1 | Differs from HEAD only by the 2 added lines (tracing-appender, tracing-subscriber) that were present at session start, from another slice. This slice's `--locked` commands all succeeded |

Deviations: Cargo.toml, lib.rs and error.rs were not touched (Win32_System_Com and Win32_UI_Shell are already enabled in the workspace windows dependency; the existing MOTW variants suffice). `tests/motw.rs` has a file-level `allow(clippy::expect_used, clippy::unwrap_used)` for helpers outside `#[test]`. The direct ADS write uses std OpenOptions on a verbatim path (no unsafe); unsafe is only in the COM calls, each with a SAFETY comment. Extra tests: record_full, referrer omitted or idempotent, missing base file not created, paths over 260 characters, and IAE success, policy, AV-removed and failure-fallback cases. API additions: AttachmentExecutor trait, IaeRequest, IaeFailure, MotwService::{start, with_executor, protect, is_poisoned}, and `pub fn iae_failure_from_hresult`. Any CheckPolicy failure maps to BlockedByPolicy; Save() failures map to BlockedByPolicy only for INET_E_SECURITY_PROBLEM or access denied (the constant was corrected during review, see SECURITY_REVIEW S1.6). The client GUID is hard-coded (7b3f1c52-9e4a-4d18-a6c7-2f8e5b0d91a3). Drop detaches rather than joins a stuck worker.

Re-review: the re-reviewer reported `cargo test --locked -p forgedm-platform --test motw` as 20 passed, 0 failed, 1 ignored (reviewer report, not a coordinator run).

Open: group-policy and antivirus behaviour is not verified against a real policy or AV; only the HRESULT mapping is unit tested. UnsupportedFs (needs a FAT volume) and long-path (260 characters or more) IAE behaviour remain unverified. The worker-skip regression test is timing-based and probabilistic. Strict host and port parsing and a sanitising ZoneInfo constructor (LOW) are not done. The Cargo.lock diff predates this slice.

NOT RUN (plan_tests_missing): none.

### S1.8 Agent bootstrap and shared pipe-name/data-root helpers (2026-11-15)

RED: not captured (red field empty). The regression test for the reported hash bug was not run against the old code.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-ipc -- --check` | 0 | clean |
| `cargo clippy -p forgedm-ipc --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-ipc --locked` | 0 | 43 passed, 0 failed, 0 ignored: bootstrap 6, identity 7, paths 9, subscribe 3, transport 18 |
| `git diff --exit-code Cargo.lock` | 1 | 2-line diff that predates this slice (already modified at session start). No Cargo.toml was edited; all cargo runs used `--locked` and succeeded |

Deviations: `ensure_agent(expected_agent_path, data_dir: Option<&Path>)` returns `BootstrapError`, defined in bootstrap.rs because error.rs is outside ownership (the CLI should map all variants except Ipc to exit 15). `ensure_agent_with` and a `#[doc(hidden)]` `with_simulated_parent_env` test seam were added, because set_var needs unsafe. A timeout kills the spawned child; a non-3 early exit returns AgentExited at once; exit 3 is treated as a lost race and polling continues. Bare or relative agent paths and relative data dirs are refused. `pipe_name_for(Option<&Path>)` returns `Result<String, PlatformError>`; `root_hash` and `normalized_root` are public in ipc::paths. In the agent, config.rs keeps a `pipe_name(Option<&Path>)` wrapper and re-exports `default_data_root`; the old agent helpers were removed and their tests moved to ipc tests/paths.rs. `helper_agrees_with_the_agents_former_computation` keeps a verbatim copy of the old algorithm. `five_concurrent_bootstraps_one_agent` waits 3.5 s before shutdown (test-only leak fix). Agent discovery walks the ancestors of current_exe, so the agent must be built first.

Open: the regression test was not seen failing first (see RED). The MEDIUM fix was not re-reviewed. `hex` and `sha2` in forgedm-agent/Cargo.toml are likely unused; removing them needs a Cargo.lock change, which the lock owner must make. The Cargo.lock diff predates this slice.

NOT RUN (plan_tests_missing): none.

### S0.9 CLI skeleton (2026-11-15)

RED: with src/main.rs replaced by an empty stub (`fn main() {}`) and the other source files removed, `cargo test -p forgedm-cli --locked` gave `test result: FAILED. 0 passed; 8 failed`. All 8 tests failed: status_without_agent_exits_10_with_message, status_with_real_agent_prints_versions, list_empty_and_json_shape, unknown_subcommand_exits_2, agent_stop_shuts_agent_down, and the three extra tests.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-cli -- --check` | 0 | clean |
| `cargo clippy -p forgedm-cli --all-targets --locked -- -D warnings` | 0 | no warnings |
| `cargo test -p forgedm-cli --locked` | 0 | 17 passed, 0 failed, 0 ignored: 8 unit, 9 integration |
| `git diff --exit-code Cargo.lock` | 1 | Non-zero from the pre-existing 2-line diff (already modified at session start). The diff does not touch forgedm-cli; Cargo.toml and Cargo.lock were not edited |

Deviations: extra tests status_does_not_start_an_agent, relative_data_dir_exits_2 and agent_stop_without_agent_exits_10, plus 3 unit tests (exit-code mapping, error text, sibling agent path); after review, 2 sanitize tests, 3 paging tests against a fake page source, and 1 process test for exit 14. A relative --data-dir is rejected by the clap value parser (exit 2), matching the agent. Busy, Timeout, I/O and other transport errors map to exit 1 (Appendix A.8 has no dedicated code). `agent stop` returns once the agent acknowledges ShutdownAgentSafely; the test waits for exit 0. `list` pages through ListDownloads at limit 500 via a private PageSource trait. The regression tests were added after the fixes and were not run red first.

Open: exit 12 (protocol mismatch) has only the unit mapping test; a process-level test needs a fake server. The Cargo.lock diff is a pre-existing 2-line change from outside this slice; the coordinator should confirm it.

NOT RUN (plan_tests_missing): none.

### S0.10b Desktop logging and shared pipe name (W3 follow-up) (2026-11-15)

RED: not recorded (red field empty). Tests were written with their fixes and were not seen failing against the old code (all findings are LOW).

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-desktop -- --check` | 0 | clean |
| `cargo clippy -p forgedm-desktop --all-targets --locked -- -D warnings` | 0 | clean. The first attempt failed on print_stderr for an eprintln!, replaced with writeln! to stderr |
| `cargo test -p forgedm-desktop --locked` | 0 | 17 passed, 0 failed, 0 ignored: lib 5, agent_client 3, config 2, logging 7 |
| `git diff --exit-code Cargo.lock` | 1 | The diff is only the 2 coordinator-added lines (tracing-appender, tracing-subscriber). This slice did not edit Cargo.lock |

Deviations: the two whoami parsing unit tests were removed with the code they tested and replaced by default_pipe_name_is_local_and_user_scoped. The four S0.10 Rust test names still exist and pass. New src/logging.rs with init, open_file_sink, subscriber_with, default_filter, log_dir_for, LoggingError and LogGuard. Logs are JSON lines in a daily rolling file (14 files kept). Fallback when the file sink fails: a stderr subscriber plus one warn!; release builds use windows_subsystem=windows, so stderr is not visible there. lib::run now takes the LogGuard as a parameter (signature change); main.rs passes it in.

Open: the Tauri exit-event guard drop is not covered by an automated test (needs a running event loop). The init() fallback path is untested because it installs a global subscriber; the pieces it uses are tested. URL redaction holds by construction and is not enforced by a test. The Cargo.lock diff is the coordinator's earlier edit.

NOT RUN (plan_tests_missing): none.

### W4 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S1.5, S1.6, S1.8, S0.9 and S0.10b; their RED lines are in the sections above.

| Command | Exit | Result |
|---|---|---|
| `git diff Cargo.lock` (pre-gate check) | 0 | Differs from HEAD by exactly 2 added lines, tracing-appender and tracing-subscriber, inside the forgedm-desktop dependencies block; matches the expected delta |
| `cargo fmt --all -- --check` (CARGO_TARGET_DIR unset) | 0 | no formatting differences |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | clean; finished in 7.58 s with no warnings |
| `cargo test --workspace --locked` | 0 | 448 passed, 0 failed, 1 ignored (sum of all test result lines). The ignored test is forgedm-platform tests/motw.rs `iae_from_sta_writes_zone_identifier` (#[ignore] by design: it uses the real shell attachment service and depends on antivirus). Memory sampled every second: no test process went above 20 MB. The one process over 1 GB was the MSVC linker link.exe (1,394 MB) during compilation; it is a build tool and was not killed. No process was killed |
| `git diff --exit-code Cargo.lock` | 1 | Expected: only the 2 pre-existing tracing lines (numstat 2 0). Re-checked after the tests, the tauri build and cargo build: unchanged |
| `node --version` | 0 | v26.13.1. The node26 shim runs Node 26 with the v25.5.0 pnpm.cjs and reports pnpm 10.29.2 |
| `pnpm install --frozen-lockfile` | 0 | lockfile up to date; 2 workspace projects already up to date |
| `pnpm -r lint` | 0 | eslint on @forgedm/desktop passed |
| `pnpm -r typecheck` | 0 | tsc --noEmit on @forgedm/desktop passed |
| `pnpm -r test` | 0 | vitest v5.1.0: 3 test files, 14 tests passed |
| `pnpm audit --prod` | 0 | No known vulnerabilities found |
| `pnpm --filter @forgedm/desktop tauri build --debug --no-bundle` | 0 | Frontend built; Rust dev profile finished in 46.10 s; built target\debug\forgedm-desktop.exe |
| `ls -la target/debug/forgedm-desktop.exe` | 0 | Exists, 13,882,880 bytes |
| `cargo build --workspace --bins --locked` | 0 | Dev profile finished in 30.18 s; target\debug holds forgedm.exe, forgedm-agent.exe, forgedm-desktop.exe and forgedm-fixture.exe |
| `forgedm --data-dir <tmp> status` (before agent start) | 10 | "forgedm: the agent is not running", exit 10 as expected |
| `forgedm-agent --data-dir <tmp>` (background) | 0 | Started as pid 45040, about 19.5 MB working set |
| `forgedm --data-dir <tmp> status` | 0 | agent 0.1.0 (pid 45040), protocol 1.0, state ready, 0 active / 0 queued, proxy system, data dir matches the temp dir |
| `forgedm --data-dir <tmp> list --json` | 0 | `{"items":[],"total":0}` |
| `forgedm --data-dir <tmp> agent stop` | 0 | "agent is shutting down"; the agent then exited on its own (AGENT_EXIT=0) |
| `Get-Process forgedm-agent` and `forgedm --data-dir <tmp> status` (post-stop) | 10 | No forgedm-agent process remained, so nothing needed killing. Status returned "the agent is not running", exit 10 as expected. Data dir holds forgedm.db, logs\agent.log.2026-11-15 and run\agent.lock |
| `git status --short` (post-gate) | 0 | Matches the pre-gate snapshot; the gate made no edits and the tauri build changed no tracked files |

Gate all_green: true. Workspace tests: 448 passed, 0 failed, 1 ignored. Fixes applied during the gate: none. Problems: none.

NOT RUN (plan_tests_missing): none listed for S1.5, S1.6, S1.8, S0.9 or S0.10b (all empty). Not in the W4 gate command list, so not run: `cargo deny --locked check` and coverage (cargo-llvm-cov). The 80% coverage target is therefore not verified for this wave. The ignored real-IAE test was run in S1.6 with `--include-ignored` (21 passed) but not in this gate.

