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

### S1.7 Single-stream vertical slice through agent and CLI (2026-11-15)

RED: not captured. The tests and implementation already existed from an interrupted attempt, so a RED-first run could not be reproduced honestly. The only failure observed was the initial `cargo clippy -D warnings` run on `crates/forgedm-cli/tests/e2e_single_stream.rs` (3 expect/unwrap lints; exit not recorded). It is fixed. No RED run was captured for the new tests, which were written with their fixes.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-core -p forgedm-agent -p forgedm-cli` | 0 | formatted |
| `cargo clippy -p forgedm-core -p forgedm-agent -p forgedm-cli --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-core --locked` | 0 | all suites pass, 0 failed |
| `cargo test -p forgedm-agent -p forgedm-cli --locked` | 0 | agent 66 unit, 9 enqueue_e2e, 2 hub, 13 process; cli 22 unit, 9 cli_basic, 5 e2e; 0 failed |
| `cargo fmt -p forgedm-core -- --check` | 0 | clean |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

Per-suite counts are copied from filtered test output. The slice's own total was not summed by the implementer, so no slice total is recorded here. The workspace total for this wave is in the W5 wave gate section below.

NOT RUN (plan_tests_missing): none reported. Not run: coverage (cargo-llvm-cov). No coverage was measured for S1.7. The CLI paths have unit tests only; no CLI e2e test covers the restart notice or a paused download.

Deviations:
- Resumed work: the interrupted attempt had already written essentially all of the slice. It was inspected, built and run. The one change was a file-level `#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]` in `crates/forgedm-cli/tests/e2e_single_stream.rs`, as the sibling test files do. No lint was relaxed elsewhere and no test was altered.
- The MEDIUM fixes were applied in forgedm-agent and forgedm-cli as well as forgedm-core, because the findings sit in those crates and they are in ownership.
- `watch` and `add --watch` end a download paused with `disk_full` or `destination_unavailable` with exit 13. A.8 defines no exit code for paused. This extends the `needs_*` precedent and is a design decision for the coordinator to confirm.
- `follow` reads `DownloadDetail.recent_events` and prints the restart notice on a durable `restart_from_zero` event; the transient `state_reason` check is kept. The CLI has its own literal for the event kind, not a shared constant.
- The expected SHA-256 lookup fails closed: a missing enqueued event or unreadable JSON returns StoreError. A unit test was added.
- `stage()` deletes the new staging file by handle when probe-fact persistence fails (new `save_facts` helper in `engine/task.rs`). No dedicated test; a store failure cannot be injected cheaply.
- `writer.rs` logs a writer-thread join failure at error level and still returns the frontier. Its module doc now says writes are positional and the cursor is not relied on.
- The review findings come from reviewer reports supplied with this slice. The implementer did not run those agents, and they are not coordinator runs. Several reviewers reported that they did not run cargo.
- `crates/forgedm-agent/src/events_hub.rs` is 844 lines, over the 800-line soft ceiling. Tests make up the module from line 463 onward.

Open:
- S1.7-silent-failure-hunter-14: the proper fix is a `checksum_expected` column writer in forgedm-storage, which is outside ownership. The agent-side fix fails closed for now. Side effect: once the 200-event retention prunes the enqueued event, such a job fails to load. That is an error instead of a silent skip of the checksum.
- Not fixed (LOW, optional): S1.7-rust-reviewer-1, -4, -5; S1.7-silent-failure-hunter-15, -16, -17, -18, -19, -21; S1.7-security-reviewer-8, -9, -10; S1.7-code-reviewer-12, -13 (events_hub.rs size).
- No re-review was recorded for S1.7 (re-review null).

### W5 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S1.7; its RED note is in the section above.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt --all -- --check` (first pass, CARGO_TARGET_DIR unset, default target dir) | 0 | no formatting differences |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` (first pass) | 0 | clean, zero warnings; finished in 20.69s |
| `cargo test --workspace --locked` (first pass) | 101 | compile failure: E0432/E0433 "could not find error/ports/client/engine/writer in forgedm_core" in forgedm-agent (lib and lib test) and in the forgedm-core test single_stream. Cause: a stale build artifact, not the source. The forgedm-core lib unit under `target/debug/build` was built at 01:42:05 from a checkout without the W5 modules, and cargo judged it fresh |
| `cargo test -p forgedm-core --locked --no-run -v` (diagnostic) | 0 | with a different feature set, forgedm-core rebuilt and all its test targets compiled; the source is fine |
| `cargo test --workspace --locked --no-run -v` (diagnostic) | 101 | confirmed forgedm-agent linked the stale `libforgedm_core-a3bf5fca883fed6d.rmeta` |
| `cargo clean --locked -p forgedm-core -p forgedm-agent -p forgedm-cli` | 0 | removed 7600 files (3.4 GiB) of build output; no source touched |
| `cargo test --workspace --locked` (second pass) | 101 | everything compiled. `forgedm-protocol --test golden` failed: `golden_files_roundtrip_value_equal` gave `Error: Os { code: 3, kind: NotFound }`. Cause: a stale binary with `CARGO_MANIFEST_DIR` baked in from a separate checkout, `C:\Users\karti\Desktop\forgedm-p0gate`, which no longer exists. Units of forgedm-desktop, forgedm-protocol and forgedm-storage (x5) carried that path |
| `cargo clean --locked -p forgedm-protocol -p forgedm-platform -p forgedm-ipc -p forgedm-storage -p forgedm-fixture -p forgedm-desktop -p forgedm-core -p forgedm-agent -p forgedm-cli` | 0 | removed 20723 files (11.1 GiB) of workspace-member build output; afterwards no dep-info under `target/debug/build` references forgedm-p0gate |
| `cargo fmt --all -- --check` (rerun after clean) | 0 | no formatting differences |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` (rerun after clean) | 0 | all 9 members re-checked; zero warnings or errors |
| `cargo test --workspace --locked` (rerun after clean; CARGO_BUILD_JOBS=6, RUST_TEST_THREADS=6) | 0 | 67 test-result blocks: 535 passed, 0 failed, 1 ignored (forgedm-platform `iae_from_sta_writes_zone_identifier`, which needs the real shell attachment service). Includes forgedm-cli e2e_single_stream 5/5 (incl. add_watch_completes_prints_sha, wrong_sha_exits_13), forgedm-agent enqueue_e2e and forgedm-core single_stream |
| `git diff --exit-code Cargo.lock` | 0 | unchanged (checked before the build and again at the end) |
| `cargo build --workspace --bins --locked` | 0 | built forgedm.exe, forgedm-agent.exe, forgedm-desktop.exe and forgedm-fixture.exe in 14.52s |
| `target\debug\forgedm-fixture.exe serve --port 0` (background) | 0 | pid 8796, printed base `http://127.0.0.1:58352`; killed at the end of the smoke |
| `target\debug\forgedm-fixture.exe hash --seed 42 --size 10485763` | 0 | `8249c7229eb3f629303c4dcc7d8b9c0b3d1c06d0763739fb3aec54dd2d681af6` |
| `target\debug\forgedm.exe --data-dir <smoke data> add <fixture URL>/f/42/10485763 --dir dl-ok --sha256 <hash> --watch` | 0 | no agent was running; bootstrap auto-started it (forgedm.db, run\agent.lock and logs created). queued -> probing -> downloading -> verifying -> finalizing -> completed; printed final path and sha256 |
| verify dl-ok (size, `Get-FileHash -Algorithm SHA256`, `Get-Content -Stream Zone.Identifier`, `*.forgedm.part`) | 0 | file is 10485763 bytes; SHA256 matches the expected hash; Zone.Identifier has ZoneId=3 and a query-free HostUrl; 0 `.part` files left |
| `forgedm ... add <fixture URL>/f/42/10485763 --dir dl-badsha --sha256 0000...0000 --watch` | 13 | queued -> probing -> downloading -> verifying -> failed. stderr: `the download ended: failed (checksum_mismatch): the SHA-256 checksum does not match`. The `.part` was kept (10485763 bytes) |
| `forgedm ... agent stop` | 0 | "agent is shutting down"; the agent log shows "agent stopped" |
| `Get-Process forgedm*` after agent stop and fixture kill (+3 s); `tasklist` grep for forgedm, cargo and rustc | 0 | 0 forgedm processes left; no cargo or rustc left (grep exit 1 = no match) |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1` | 0 | 13 PASS, 0 FAIL, 0 NOT RUN (no-tls-bypass, no-assert-sql-safe, unsafe-only-in-platform, command-new-allowlist, layering, no-fixture-in-shipped, no-fault-injection-in-release, no-reqwest-compression, tauri-versions-match, no-loose-versions, lockfiles-committed, manifest-asinvoker, eslint-bans) |

Gate all_green: true. Workspace tests: 535 passed, 0 failed, 1 ignored. Fixes applied during the gate: no source, test, manifest or lockfile changes. Only build output was cleaned. The two `cargo test --workspace` runs that exited 101 before the clean were stale-artifact compile failures; they are recorded above and are not counted as results.

Problems and notes:
- Process hazard, resolved for this gate: a run from the separate checkout `C:\Users\karti\Desktop\forgedm-p0gate` wrote into this repo's target dir (`C:\Users\karti\Desktop\IDM Clone\target`) at about 01:42. It left fingerprints that poisoned workspace members. Any future out-of-tree copy must use its own CARGO_TARGET_DIR. That build overlapped the W5 slice builds and may have added to the load on the PC; this cannot be confirmed from the artifacts.
- Observation, not a failure: the smoke file's Zone.Identifier ends with a 0x00 byte and has no trailing CRLF (71 bytes). The agent attributes the rewrite to IAttachmentExecute in apply_motw. ZoneId=3 and a query-free HostUrl are present. The only ignored test covering this real-shell path (`iae_from_sta_writes_zone_identifier`) was not run in this gate, so the smoke is its only live evidence here.
- Not run: `cargo deny --locked check` and coverage (cargo-llvm-cov) are not in the gate command list. No coverage was measured for W5. The gate report's own NOT RUN list was truncated, so no further items are recorded.

### S1.9 Probe, validated resume, pause/resume/cancel/remove, checkpoints: FIRST PROOF (2026-11-15)

RED: compile-time RED before any implementation: `cargo test -p forgedm-core --locked --test checkpoint` gave E0432 unresolved import `forgedm_core::checkpoint`; `--test probe` gave E0432/E0433 for `forgedm_core::probe`; `--test resume` gave E0432 for `forgedm_core::engine::Control` and `ports::StoredProgress`, plus E0599 `control` not found on EngineHandle. Runtime RED: the record is cut off after `cargo test -p forgedm-cli --lo`, so the runtime RED result is not recorded here. Unit tests added to resume.rs after the fix were not red-first; no RED run was recorded for them.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-core -- --check` | 0 | clean |
| `cargo clippy -p forgedm-core --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-core --locked` | 0 | all suites ok, 0 failed (output grep-filtered) |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

Per-suite counts were not summed by the implementer. The slice's tests block shows 0 passed, 0 failed, 0 ignored only because no total was taken; those zeros are not counts. The W6 wave gate section below gives the workspace total.

NOT RUN (plan_tests_missing): none reported. Not run: coverage (cargo-llvm-cov), which was not requested. Coverage was not measured for S1.9.

Test names: 18 of the 20 plan test names exist with the exact name. `200_on_resume_never_written_at_offset` and `416_at_end_after_crash_is_complete` are implemented as `n200_...` and `n416_...`, because a Rust identifier cannot start with a digit. This mapping is recorded here.

Deviations:
- `416_at_end_after_crash_is_complete` works as follows: a crash left every byte committed, so the resume sends `Range: bytes=T-T` with If-Range. The fixture answers 416 (`bytes */T`), decide() returns SegmentComplete and the download goes to verifying. A matching check also asserts that no file byte was transferred (only the short 416 message body).
- S1.7 tests were adjusted because S1.9 changes behaviour legitimately: the probe now finds range-capable fixtures resumable, and each run makes HEAD plus GET 0-0 before the transfer GET. In single_stream.rs the expected mode is SingleRanged, unknown_length uses `ranges=0&cl=0`, the redirect hop count is 3*4, identity_header_sent uses a stub that serves several connections, and the interrupted-restart test uses `ranges=0`. In agent enqueue_e2e, nonresumable_kill_restart_redownloads_from_zero uses `ranges=0` and counts transfer GETs. Assertions were rewritten for the new behaviour, not deleted.
- TransferMode::Segmented is probed for files of 8 MiB or more, but S1.9 stores and runs it as single_ranged (one range stream). Segmenting arrives with S1.11.
- Retry{restart_from_zero} and every restart delete the stale .part by handle (only if provably ours) and recreate it, instead of truncating it in place. Progress, validators and the part record are cleared in one storage transaction (new Downloads::reset_for_restart).
- RangeDecision::RangeUnsupported and ::Reprobe fail with range_protocol_violation (ranges_no_longer_supported, reprobe_needed) instead of re-planning or doing one re-probe. A 403, 404 or 410 on a resume fails with http_status; needs_url_refresh is not produced (no TaskStop variant for it).
- The client has no total timeout (ADR-007 and clippy ban RequestBuilder::timeout), so the 5 s HEAD deadline is a tokio::time::timeout around the whole HEAD in probe.rs.
- The pause path persists `pausing` only after the transfer has quiesced and checkpointed (settle), not before the stop is requested.
- The expected SHA-256 is now the downloads.checksum_expected column (storage insert_with_checksum). Rows written by S1.7 that only have the enqueued-event form are still read through a legacy fallback. No migration was needed.
- The fixture gained a GET /__log JSON export (it also reports served_bytes), a verify-prefix subcommand and a verify::prefix_matches function. The fixture's slow throttle is capped at 640 KiB/s, so the proof uses slow_from near the end of the file to get a throttled tail. The kill therefore lands at the first checkpoint after 2 s, at 91-96% committed rather than just over 30%. The 30% condition is still the wait threshold.
- The agent now advertises the resume capability in the handshake (A.2: advertised once implemented).
- The log file name date is documented as UTC (tracing-appender rolls at 00:00 UTC) and pinned by a test, instead of switching to local time.
- The IPC-03 seam is Server::bind_expecting (doc(hidden)); bind() is unchanged.
- Reviewer reports were supplied with this slice. The implementer did not run those agents, and they are not coordinator runs. The reviewers did not run cargo; they read the code only.

Open:
- LOW findings not touched: S1.9-rust-reviewer-1, S1.9-rust-reviewer-2, S1.9-code-reviewer-3, S1.9-silent-failure-hunter-7 and S1.9-silent-failure-hunter-8 (not fixed). S1.9-code-reviewer-4 and S1.9-silent-failure-hunter-5 need no change (see SECURITY_REVIEW S1.9).
- No re-review was recorded for S1.9 (re-review null). The MEDIUM fix, S1.9-silent-failure-hunter-6, has not been re-reviewed.

### W6 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S1.9; its RED note is in the section above.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt --all -- --check` (CARGO_TARGET_DIR unset, default repo target) | 0 | no formatting differences |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | clean, no warnings; finished in 11.35s |
| `cargo test --workspace --locked` | 0 | 74 test binaries including doc-tests: 592 passed, 0 failed, 1 ignored (forgedm-platform `motw::iae_from_sta_writes_zone_identifier`, a pre-existing ignore: "talks to the real shell attachment service; depends on antivirus"). W6 suites present and green: agent control_e2e 3, pipe_fatal 2, enqueue_e2e 9, process 13; cli e2e_first_proof 2, cli_basic 11, unit 26; core checkpoint 2, probe 3, recovery 4, resume 13, single_stream 17, unit 42; storage downloads 12, segments 9; ipc transport 19; fixture cli 3 |
| `git diff --exit-code Cargo.lock` | 0 | unchanged (checked after the tests and again after all scripts) |
| `cargo build --workspace --bins --locked` | 0 | built forgedm.exe, forgedm-agent.exe, forgedm-desktop.exe and forgedm-fixture.exe in target\debug in 12.75s; the rerun before the proof finished in 0.52s with no rebuild |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1` | 0 | 13 PASS, 0 FAIL, 0 NOT RUN: no-tls-bypass, no-assert-sql-safe, unsafe-only-in-platform (65 constructs inside platform, 0 outside), command-new-allowlist, layering (9 packages, 17 edges), no-fixture-in-shipped, no-fault-injection-in-release, no-reqwest-compression, tauri-versions-match, no-loose-versions, lockfiles-committed, manifest-asinvoker, eslint-bans |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows-smoke.ps1` | 0 | 22 PASS, 0 FAIL, 0 NOT RUN: build-bins (its own `cargo build -p agent -p cli`, exit 0), agent-starts, agent-has-no-tcp-listener (0/0/0 rows), second-agent-same-data-dir-exits-3, agent-peak-working-set-under-1gb (30.1 MB), agent-stop-exits-0, restart-after-stop, no-leftover-processes; temp dir removed |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\phase1-proof.ps1` | 0 | FIRST PROOF PASS, 15 checks PASS, 0 FAIL. Fixture file 256 MiB, seed 77, throttled from byte 255852544. `killed the agent at committed=256966656 accepted=256966656 of 268435456` (state downloading). DB committed after restart: 256966656. Resumed GET 1 `range bytes=256966656-268435455 if-range "77-268435456"` (2 transfers total; the first started at byte 0). `served 268500993, bound 274726912` and `served 268500993 of size 268435456` (65,537 bytes over size). Get-FileHash and the fixture hash are both b2130714ca01ab3f22bfa0c15fc61683674d6dded38797e711930596860b8f96; agent-recorded digest PASS. verify-prefix before and after restart: OK. Final line: `PASS  FIRST PROOF: kill, resume, identical bytes` |
| `tasklist \| grep -i forgedm; Get-Process forgedm-agent,forgedm-fixture,forgedm` | 1 | grep exit 1 means no match: no forgedm-agent, forgedm-fixture or forgedm process left after the scripts; nothing needed killing |
| `Get-ChildItem %TEMP% -Filter 'forgedm-*'` | 0 | empty: the proof's forgedm-phase1-proof-<guid> temp dir (about 256 MiB) and the smoke temp dir were both deleted |

Gate all_green: true. Workspace tests: 592 passed, 0 failed, 1 ignored. Fixes applied during the gate: none.

Problems and notes:
- Observation, not a failure: the kill landed at 256966656 of 268435456 bytes (about 95.7%), inside the throttled 12 MiB tail. The proof requires at least 30% committed, so this passes, but the resume covered only the last 11,468,800 bytes. A run that killed earlier in the file was not exercised.
- The proof reports 268,500,993 bytes served against a 268,435,456-byte file (65,537 bytes over). The record does not explain the excess, and it was not analysed in this gate.
- The proof cannot detect a missing FlushFileBuffers, because TerminateProcess leaves the OS file cache intact. The script says so itself. Power-loss durability: NOT RUN.
- windows-smoke.ps1 runs its own `cargo build --locked -p forgedm-agent -p forgedm-cli --bins` (exit 0). It ran one at a time, never alongside another build.
- NOT RUN, as instructed: the tauri build and pnpm. Coverage (cargo-llvm-cov) was not run, as the task did not ask for it.

### S1.10+ Usable desktop MVP and per-user NSIS installer (2026-11-15)

RED: Rust: `cargo test -p forgedm-desktop --locked --no-run` exited 101 with `could not compile forgedm-desktop (lib test) due to 7 previous errors`. The new tests referenced modules and variants that did not exist yet (request, open_folder, engine, CommandErrorKind::InvalidRequest). TS: `pnpm test` with all new test files and no implementation gave `Test Files 9 failed (9), Tests 38 failed | 9 passed (47)` (exit code not recorded). Before the fixes, `pnpm vitest run src/App.resilience.test.tsx` exited 1 with 3 of 3 new tests failing.

| Command | Exit | Result |
|---|---|---|
| `pnpm vitest run src/App.resilience.test.tsx` (RED, before the fixes) | 1 | 3 of 3 new tests failed |
| `pnpm test` | 0 | 10 files, 72 tests passed |
| `pnpm typecheck` | 0 | tsc --noEmit clean |
| `pnpm lint` | 0 | eslint clean. The first run had a react-hooks/immutability error, fixed by making the recursive refresh a named function expression |
| `cargo fmt -p forgedm-desktop -- --check` | 0 | clean |
| `cargo clippy -p forgedm-desktop --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-desktop --locked` | 0 | all test binaries ok, 0 failed. The Rust code was not changed this round. Rust test counts were not tallied; the 72 above is the vitest count |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

Deviations:
- The sidecars (externalBin) and the installer settings are in a separate overlay, `src-tauri/tauri.bundle.conf.json`, not in tauri.conf.json. tauri-build checks externalBin files at compile time, so the base config would break cargo test, clippy and tauri build on a clean checkout. The overlay holds externalBin [forgedm-agent, forgedm], the nsis currentUser install mode and `beforeBuildCommand` `pnpm run prepare-sidecars && pnpm run build`. The installer is built with `pnpm run package`. The base config keeps targets [nsis], the CSP, freezePrototype, withGlobalTauri false and useHttpsScheme false.
- The sidecar step is in beforeBuildCommand, not beforeBundleCommand, because tauri-build needs the files before cargo compiles the desktop crate.
- Added a get_snapshot command for the plan's 1 Hz polling (in COMMAND_NAMES, build.rs, the capability and the permission files). list_downloads and agent_status are kept. The existing App.test.tsx mocks answer get_snapshot now; assertions and test names are unchanged.
- Added CommandErrorKind::InvalidRequest (snake_case `invalid_request`) for a malformed id, or a show_in_folder target that is not completed or is missing. Server error messages are fixed per error code and the agent's own text is never relayed.
- The logging.rs test is renamed desktop_code_spawns_no_subprocess_except_the_open_folder_helper. It still forbids whoami, tokio::process, ShellExecute, cmd.exe and powershell; it allows Command::new only in open_folder.rs, exactly once.
- The desktop build.rs sets an explicit asInvoker manifest: Tauri's default (Common Controls v6) plus requestedExecutionLevel asInvoker with uiAccess false.
- Tauri command arguments use snake_case (target_dir, discard_progress, restart_from_zero, delete_partial).
- The validate test case `http:///x` was removed, because WHATWG parsing accepts it with host x. The engine remains the authority on URLs.
- The Tauri uninstaller's app-data deletion applies to the identifier directory (dev.forgedm.desktop) only. Agent data in %LOCALAPPDATA%\ForgeDM is left alone. This rests on documented Tauri behaviour and is not tested. The note is in config.rs because JSON has no comments.
- A stopped engine (agent_unavailable) still replaces the view with the Start banner, because nothing can be done until it starts.

Open:
- NOT RUN: the NSIS per-user installer. The bundle step was not run; no `*-setup.exe` exists (see the W7 wave gate).
- LOW findings not fixed: typescript-reviewer-3, react-reviewer-7, react-reviewer-9, security-reviewer-11, rust-reviewer-12, code-reviewer-15. The pending-refresh flag has no dedicated test.
- No re-review is recorded for the two MEDIUM fixes (react-reviewer-5 and -6).

NOT RUN (plan_tests_missing): none reported.

### S1.11 Segmented transfer with work-splitting (2026-11-15)

RED: the implementation was written before the tests, so the TDD order was not followed and most of the slice has no RED run. The one RED run captured: `cargo test -p forgedm-core --locked --test segmented restart_keeps` with the fix temporarily reverted exited 101 (`a cap of 2 must survive the restart, but 8 requests were made`). The fix was then restored.

| Command | Exit | Result |
|---|---|---|
| `cargo test -p forgedm-core --locked --test segmented restart_keeps` (fix temporarily reverted) | 101 | RED: `a cap of 2 must survive the restart, but 8 requests were made`. Fix restored afterwards |
| `cargo fmt -p forgedm-core -- --check` | 0 | clean |
| `cargo clippy -p forgedm-core -p forgedm-agent --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-core --locked` | 0 | 241 passed, 0 failed, 1 ignored (the 5 GiB test). The segmented suite has 15 tests, including the new regression test restart_keeps_the_stored_connection_cap |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

Deviations:
- Edits outside the owned path list, all additive and needed to persist and resume segment tables: core ports.rs (checkpoint_segments defaults to refusing; load_segments defaults to empty); core testing.rs (MemoryStore stores segments); agent store_adapter.rs (implements both over the segment save and load, and maps record.max_connections into the new field); agent engine_host.rs (EngineControl::set_max_connections, default not_implemented, plus the EngineHost impl). StoredJob gained max_connections in ports.rs and in every constructor (engine/cleanup.rs, testing.rs, a resume.rs unit test, tests/resume.rs). forgedm-storage, Cargo.toml and Cargo.lock were not changed.
- forgedm-cli/tests/e2e_first_proof.rs (the S1.9 first proof, outside the owned paths) was adapted minimally. It verifies each committed range against the generator, splits requests at the kill, and requires each resumed GET to start at or after its own segment's committed offset. The kill, hash and served-bytes bound assertions are unchanged.
- core tests/single_stream.rs single_stream_matches_generator: the expected mode for sizes of 8 MiB or more changed from SingleRanged to Segmented (the 10 MiB+3 case). This is the plan's behaviour change.
- New transfer/prealloc.rs (sparse, FAT32 limit, part reopen without truncation) and engine/task/segmented.rs for the task glue.
- open_part_for_segments repeats the ~20 lines of file checks from resume::open_valid_part instead of editing resume.rs, which is not owned. It does not truncate, because other segments live beyond the first gap.
- UpdateLimits is implemented only for download scope with max_connections. Global scope, rate_limit_bps and max_active_downloads answer Unsupported/not_implemented (S1.13). A queued download starts with the new cap. The stored cap now survives an agent restart (rust-reviewer-1 fix); the earlier default of 8 for recovered jobs no longer applies.
- Cap entries live in an in-memory map for the process lifetime (bounded by the queue cap) and are dropped on remove.
- logged_ranges_disjoint_and_cover checks requested ranges: no gaps, no duplicate starts, full cover to T, and an exact tiling only when no split happened. File content is proven by the sha and file_matches checks.

Open:
- Live UpdateLimits max_connections changes are session-only. Persisting them needs a forgedm-storage update method, which is outside the S1.11 ownership.
- `cargo test -p forgedm-cli` was not rerun this round. The only agent and CLI facing change is the StoredJob field; forgedm-agent passes clippy --all-targets.
- The unthrottled segmented proof run exited 1 on its 8-connection check (see the W7 wave gate). The throttled run passed.
- NTFS-dependent tests ran for real on this volume (sparse_set_then_cleared passed).

NOT RUN (plan_tests_missing): none reported.

### W7 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S1.10+ and S1.11; their RED lines are in the sections above.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt --all -- --check` (CARGO_TARGET_DIR unset, shared default target dir; cargo and rustc 1.100.0) | 0 | no formatting differences |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | clean, no warnings |
| `cargo test --workspace --locked` (CARGO_BUILD_JOBS=6, RUST_TEST_THREADS=6) | 0 | 80 suites including doc-tests: 652 passed, 0 failed, 2 ignored (full_5gib_segmented_exact_size_stream_compare, which writes 5 GiB, and iae_from_sta_writes_zone_identifier). New W7 suites ran: segmented 15, segment_table_props, large_offsets, and the desktop engine, open_folder and requests suites |
| `git diff --exit-code Cargo.lock` | 0 | unchanged, checked at the end too. pnpm-lock.yaml, root package.json and root Cargo.toml also unchanged |
| `node --version` (PATH set to the node26 shim and nvm v26.13.1) | 0 | v26.13.1; pnpm resolves to the node26 shim wrapper, version 10.29.2 |
| `pnpm install --frozen-lockfile` | 0 | lockfile up to date |
| `pnpm -r lint` | 0 | no errors |
| `pnpm -r typecheck` | 0 | tsc --noEmit clean |
| `pnpm -r test` | 0 | 10 test files, 72 tests passed |
| `pnpm audit --prod` | 0 | no known vulnerabilities found |
| `pnpm --filter @forgedm/desktop tauri build --config src-tauri/tauri.bundle.conf.json --no-bundle` | 0 | release build. The overlay beforeBuildCommand ran prepare-sidecars: `cargo build --release --locked -p forgedm-agent -p forgedm-cli` (1m24s), staging both sidecars under apps\desktop\src-tauri\binaries. The desktop release build finished in 2m09s. The NSIS bundle step was deliberately not run |
| `ls target\release` | 0 | forgedm-desktop.exe 11,243,520 bytes; forgedm-agent.exe 10,458,112 bytes; forgedm.exe 2,157,568 bytes |
| `mt.exe -nologo -inputresource:<exe>;#1 -out:<file>` for each release exe, then grep for requireAdministrator or highestAvailable | 0 | each embedded manifest has requestedExecutionLevel level="asInvoker" uiAccess="false"; the desktop manifest also carries Common-Controls v6; no requireAdministrator or highestAvailable. The record also says the overall shell exit was 1, caused by `grep -c` returning 1 on zero matches, not by a failed check |
| `cargo build --workspace --bins --locked` | 0 | dev profile finished in 11.41s; target\debug holds forgedm.exe, forgedm-agent.exe, forgedm-desktop.exe and forgedm-fixture.exe |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1` | 0 | 13 PASS, 0 FAIL, 0 NOT RUN. command-new-allowlist has 2 hits (bootstrap.rs and open_folder.rs). manifest-asinvoker PASS for debug and release forgedm.exe, forgedm-agent.exe and forgedm-desktop.exe, and for debug forgedm-fixture.exe (no manifest string) |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows-smoke.ps1` | 0 | 22 PASS, 0 FAIL, 0 NOT RUN |
| Segmented proof run A (ad hoc script in the session scratchpad, not in the repo): unthrottled 64 MiB fixture, `forgedm add --connections 8`; run 4 times | 1 | Bytes correct: length 67121209, SHA-256 equal to the fixture hash and the agent digest, no .part left, transfer_mode segmented. Four range GETs (0-16777215, 16777216-33554431, 33554432-50331647, 50331648-67121208) are disjoint and cover the file, but all used conn_id 0: one connection, nothing in parallel. At most one agent-to-fixture TCP connection was sampled. The script's "8 distinct connections" check failed, which is the cause of exit 1. Agent log: worker 1 took all 4 segments; workers 2-8 ended NoWork. agent stop exited 0 |
| Segmented proof run B (same file, `-ExtraQuery 'slow=4000000'`, 4 MB/s per response) | 0 | PASS. 8 distinct conn_ids (0-7) carried range GETs and 8 agent-to-fixture TCP connections were sampled established at once. 4 initial ranges plus 4 work-steal splits; the effective ranges tile [0, 67121209) with no gaps, and every overlap is a full nesting. served_bytes 67383354 (size + 262145). SHA-256 matched. agent stop exited 0 |
| `tasklist \| grep -i forgedm` | 1 | no forgedm process left (grep no match) |

Gate all_green: false, as recorded by the gate run. Workspace tests: 652 passed, 0 failed, 2 ignored. Fixes applied during the gate: none.

Problems and notes:
- Segmented proof run A exited 1 (see the row above). Run B passed. The unthrottled download used one connection, so the parallel-connection behaviour is shown only by the throttled run B.
- The NSIS per-user installer was NOT built. This machine has no NSIS toolchain (%LOCALAPPDATA%\tauri does not exist and there is no makensis). The Tauri 2.12.4 bundler would download the NSIS zip and nsis_tauri_utils.dll from GitHub into %LOCALAPPDATA%\tauri\NSIS. These were not downloaded: installing tooling needs the user's permission, and the rules forbid installs.
- The problems text in the record was cut off after the NSIS point, so any further problems are not recorded here.

NOT RUN:
- The NSIS installer (the tauri bundle step; no `*-setup.exe` was produced).
- Not in the W7 gate command list, so not run: `cargo deny --locked check` and coverage (cargo-llvm-cov). The 80% coverage target is therefore not verified for this wave.
- plan_tests_missing: none for S1.10+ or S1.11.

### W7 installer build (2026-11-16, coordinator, after user approval of the NSIS download)

| Command | Exit | Result |
|---|---|---|
| `pnpm --filter @forgedm/desktop run package` (= `tauri build --config src-tauri/tauri.bundle.conf.json`; Node 26.13.1, pnpm 10.29.2, CARGO_BUILD_JOBS=6) | 0 | Sidecars built by the release build. Tauri downloaded nsis-3.13.zip and nsis_tauri_utils.dll v0.5.3 and validated both hashes. makensis produced `target\release\bundle\nsis\ForgeDM_0.1.0_x64-setup.exe` (6,384,821 bytes). |
| `sha256sum ForgeDM_0.1.0_x64-setup.exe` | 0 | 951c5b5d07c7ef692662bd866cd93ce48601c0feeef13ed2fc109bb7329f18d1 |
| `git diff --stat Cargo.lock pnpm-lock.yaml` | 0 | no changes |

NOT RUN: installing the setup exe on this machine, and the manual run of the installed app. Both are left to the user. The installer is unsigned, so SmartScreen will warn.

### S1.12 Retries, Retry-After, host cooldown, redirects, network hardening (2026-11-15)

RED: partial. The TDD order was not clean: the core wiring (resilience, worker, engine, task) was written before the new e2e tests, so there is no RED output for the core retry tests. RED evidence exists for three items, all as reported by the implementer (exit codes not reported):
- (a) a mutation restoring the old completion rule failed with `completion waited for staggered workers: 1.1495s`; the fixed code passes in 0.18 s.
- (b) detail.rs tests failed to compile (E0425, `is_transferring` not found).
- (c) storage half: downloads tests failed to compile (E0599, no method `set_max_connections`).

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-core -- --check` | 0 | clean |
| `cargo clippy -p forgedm-core --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo test -p forgedm-core --locked` | 0 | all suites ok: 278 passed, 0 failed, 2 ignored (slow tier) |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |
| `cargo test -p forgedm-core --locked --test retry_e2e stale_timer` (with the fix temporarily disabled) | 101 | new regression test FAILED without the fix. Fix restored; the implementer reports the test then passes (that passing run is not listed as a separate command here; the W8 gate run below includes retry_e2e) |

NOT RUN (plan_tests_missing):
- `connect_timeout_15s_blackhole` stays `#[ignore]` (slow tier); not run in this round.
- `tls_untrusted_cert_fatal` and the https_to_http e2e over TLS remain NOT RUN (unchanged). The implementer reports TLS tests in forgedm-core that use the existing rcgen and tokio-rustls dev-dependencies, and the fixture TLS mode (D9) was not added. The reviewers disagree on whether the untrusted-cert test proves certificate validation (SECURITY_REVIEW S1.12-security-reviewer-7, LOW, open), so these entries are not counted as passing TLS e2e evidence.

Deviations:
- Storage ownership: besides `set_max_connections`, `Downloads::set_next_retry_at` was added in crates/forgedm-storage/src/downloads.rs. The plan requires `retry_waiting{next_retry_at_ms}` to be persisted and the existing transition() cannot store it. Coordinator to confirm.
- New DownloadStore trait methods, all with defaults: `set_max_connections`, `schedule_retry`, `next_retry_at`. New public API: `engine::start_with(config, deps, RetryEnv)`, `transfer::resilience::{Resilience, RetryEnv, Verdict}`, `engine::hosts::{HostRegistry, HostHandle}`, `client::{ClientOptions, HttpClient::with_options, retry_after_of, content_disposition_of}`, `error::{Fault, TaskStop::{Retry, RetryWait, NeedsUrlRefresh, Reprobe}}`, `ProgressSample.active_ranges`, `LiveDownload::set_active_ranges`, `SharedInit`. Other slices should know about these.
- The Retry-After per-host cooldown is waited through `Resilience::gate` before each request. Waits use the injected Sleeper, so a recording sleeper does not hold requests back. "No storm" is asserted by request counts and recorded wait durations, not by wall-clock spacing.
- Existing tests changed because behaviour changed. segmented.rs `ranges_ignored_mid_download_no_misplaced_writes`: the validator-less 200 now ends in a re-probe, a plain single-stream restart and a byte-exact completion, instead of failed(range_protocol_violation). single_stream.rs `drop_after_with_cl_is_incomplete_not_completed` now uses `ranges=0`, because with ranges the engine retries from the claim and completes (covered by `mid_chunk_drop_resumes_from_claim_no_overlap`). segmented.rs is exactly 800 lines.
- Failure behaviour changes: malformed framing now carries reason `malformed_response`. A reprobe after a broken range contract is restart-from-zero plus a fresh plan, at most once per run, not continue-with-progress.
- `classify_reqwest` treats a connect-phase io::Error of kind Other with a payload (how rustls errors arrive) or UnexpectedEof as Tls. rustls cannot be downcast because forgedm-core has no rustls dependency. retry.rs gained `HostState::extend_cooldown`.
- DNS (permanent NXDOMAIN) and connect timeout or refused get 3 limited retries, then the specific error code, not retry_budget_exhausted. `classify_transfer(DnsFailure)` stays Fatal for the pure S1.3 tests; Resilience overrides it.
- `SegmentInfo.active` is derived from the engine's live active byte ranges, which refresh within about 250 ms. It is not exact per-worker state.
- The stale-timer test seeds a RetryWaiting row and calls recover() twice rather than driving a real 130 s Retry-After, so it stays fast (about 1.8 s).

Open:
- LOW findings not addressed (optional): rust-reviewer-4 and security-reviewer-8 (is_wrapped_tls_error heuristic); rust-reviewer-5 (gate loop; a loop would spin under the fake sleeper); rust-reviewer-6; security-reviewer-7 and -9; code-reviewer-10, -11 and -12 (test tightening); silent-failure-hunter-16 (unix_ms fallback); silent-failure-hunter-17 (engine_cell.set, already commented as infallible).
- The two MEDIUM fixes (rust-reviewer-1 and silent-failure-hunter-13) have not been re-reviewed (re-review null). The silent-failure-hunter-13 fix has no test, because MemoryStore cannot inject faults.
- The reviewers read code only and ran no cargo. Their cargo results are the implementer's; the coordinator's own runs are in the W8 wave gate below.

### W8 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S1.12; its RED notes are in the section above.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt --all -- --check` (Git Bash; PATH=$HOME/.cargo/bin; CARGO_BUILD_JOBS=6; RUST_TEST_THREADS=6; CARGO_TARGET_DIR unset, shared default target dir) | 0 | no formatting differences |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | clean, no warnings; finished in 11.46 s (forgedm-core, forgedm-storage and forgedm-agent rechecked) |
| `cargo test --workspace --locked` (output kept in the session scratchpad as test.log) | 0 | 85 test-result lines including doc-tests: 694 passed, 0 failed, 3 ignored (full_5gib_segmented_exact_size_stream_compare, connect_timeout_15s_blackhole, iae_from_sta_writes_zone_identifier). W8 suites: core network_hardening 7 (16.4 s), redirects 6, retry_e2e 15 passed and 1 ignored, w7_followups 2 (21.1 s); agent persisted_limits 2. Changed suites: resume 13, segmented 15 (56.8 s), single_stream 17, recovery 4. Also green: cli e2e_first_proof 2, e2e_single_stream 5, cli_basic 11; agent unit 72, control_e2e 3, enqueue_e2e 9, process 13 |
| `git diff --exit-code Cargo.lock` (checked after the tests and again at the end) | 0 | unchanged. No diff in root Cargo.toml, clippy.toml, rust-toolchain.toml, .cargo, docs or tests/fixtures. `git status` lists the same 33 entries as at the start |
| `cargo build --workspace --bins --locked` | 0 | dev profile finished in 11.65 s; target\debug holds forgedm.exe, forgedm-agent.exe, forgedm-desktop.exe and forgedm-fixture.exe |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1` (first attempt, Git Bash quoting error) | 127 | invocation error, not a gate result: Bash stripped the backslash and PowerShell reported that 'scriptscheck.ps1' does not exist |
| `powershell -NoProfile -ExecutionPolicy Bypass -File 'scripts\check.ps1'` (rerun, path single-quoted) | 0 | 13 PASS, 0 FAIL, 0 NOT RUN: no-tls-bypass, no-assert-sql-safe, unsafe-only-in-platform (65 inside platform, 0 outside), command-new-allowlist, layering (9 packages, 17 edges), no-fixture-in-shipped, no-fault-injection-in-release, no-reqwest-compression, tauri-versions-match, no-loose-versions, lockfiles-committed, manifest-asinvoker, eslint-bans |
| `powershell -NoProfile -ExecutionPolicy Bypass -File 'scripts\windows-smoke.ps1'` | 0 | 22 PASS, 0 FAIL, 0 NOT RUN: build-bins (its own build step passed), agent-starts (pid 7272), agent-has-no-tcp-listener (0/0/0 rows, 0 UDP endpoints), second-agent-same-data-dir-exits-3, agent-peak-working-set-under-1gb (30.7 MB), agent-stop-exits-0, restart-after-stop, no-leftover-processes; temp dir removed |
| `powershell -NoProfile -ExecutionPolicy Bypass -File 'scripts\phase1-proof.ps1'` (output kept as proof.log in the session scratchpad) | 1 | 11 PASS, 3 FAIL. The run killed the agent at committed=257949696 of 268435456. FAIL: committed prefix of the .part matches the generator (fixture reported a content mismatch at byte offset 256901120). FAIL: the database kept at least what was committed before the kill (db committed 254541824). FAIL: every resumed GET starts at or after the committed offset and sends If-Range (the list included the pre-kill segment GETs). PASS: Get-FileHash equals the fixture hash (b2130714...8f96), agent digest, served 270860289 within bound 274726912, no staging file left. The implementer attributes the three FAILs to the script, which assumes one stream (contiguous committed prefix, segments[0] as the DB offset, every GET after the first as a resumed GET). The script was not changed. |
| ad hoc `scratchpad\seg-proof.ps1 -BinDir target\debug -Work scratchpad\segproof1` (segment-aware copy of the same check: same URL, seed, kill rule and bounds) | 1 | harness bug in the ad hoc script: three verification range fetches (conn 3, no If-Range) came after the kill sequence and were counted as resumed GETs. Every product check passed. Script fixed and rerun |
| ad hoc `scratchpad\seg-proof.ps1 -BinDir target\debug -Work scratchpad\segproof2` (output kept as segproof2.log) | 0 | PASS, segment-aware. Killed at committed_bytes=257949696 in segmented mode. Pre-kill segments 0+239599616, 239599616+17301504, 262209536+1048576; all 3 committed ranges match the generator. After restart the DB has the same segments, committed 257949696 >= 257949696. Six GETs before the kill. The only two agent GETs after the restart were bytes=256901120-262209535 and bytes=263258112-268435455, each starting at its segment's committed end, sending If-Range "77-268435456", and overlapping no pre-kill committed byte. Served 269877249 bytes, under bound 274726912. SHA-256 equals the fixture hash and the agent digest. No .part left; agent stop exit 0 |
| `forgedm-agent.exe --data-dir smoke1\data --proxy none`; `forgedm list --json`; `forgedm get <id> --json`; `forgedm agent stop` (retry smoke attempt 1, agent events read) | 0 | attempt-1 download: queued to probing at +3 ms, probing to downloading at +1007 ms, completed at +1064 ms; retry_count 0; checksum match; agent stop exit 0 |
| `powershell ... -File scratchpad\retry-smoke.ps1 -BinDir target\debug -Work scratchpad\smoke1` (retry smoke attempt 1) | 1 | harness bug: the script read test_key and at_unix_ms, which the fixture's /__log JSON does not export. The product part passed: `add --sha256 --watch` exited 0 in 1.1 s; the 4 MiB SHA-256 matched (bf5b8245...d888); fixture and agent were killed in finally |
| `powershell ... -File scratchpad\retry-smoke.ps1 -BinDir target\debug -Work scratchpad\smoke2` (retry smoke attempt 2) | 1 | harness bugs: on PS 5.1, Start-Process ExitCode was blank because the handle was not cached, so three exit checks showed blank; the stall "claimed offset" was read from committed_bytes, which stays 0 during a stall. The downloads completed with matching hashes. Script fixed and rerun |
| `powershell ... -File scratchpad\retry-smoke.ps1 -BinDir target\debug -Work scratchpad\smoke3` (retry smoke attempt 3; output kept as smoke3.log; request arrival timed by polling /__log every 25 ms) | 0 | PASS, retry smoke. (1) `/f/4242/4194304?status=503&retry_after=1&fail_first=2&t=retry503`: `add --sha256 --watch` exited 0 after 1.1 s; SHA-256 bf5b8245...d888 equals the fixture hash; checksum match. Request log: seq 0 HEAD conn 1 (503); seq 1 GET bytes=0-0 (503); seq 2 HEAD at +982 ms; seq 3 GET bytes=0-0; seq 4 GET bytes=0-4194303 with If-Range "4242-4194304". One roughly 1 s Retry-After wait after the failed probe; the agent events show probing to downloading at +1014 ms. (2) `/f/4343/4194304?stall_after=3145728&t=stall`: during the stall the agent reported accepted_bytes 3145728 and committed_bytes 0. Request log: seq 7 GET bytes=0-4194303 with If-Range "4343-4194304" (stalls after 3 MiB); seq 8 GET on conn 2 for bytes=3145728-4194303 at +30844 ms. The record was cut off in the middle of case (2), so its remaining detail is not recorded here and the script's exit 0 is not independently verified for that case |

Gate all_green: false. The gate command `scripts\phase1-proof.ps1` exited 1 with 3 FAILs, the script was not changed, and the gate is therefore not green. The gate record given to the coordinator is cut off in the middle of the retry-smoke stall case and has no all_green field; this status is set from the exit codes. Workspace tests: 694 passed, 0 failed, 3 ignored. No product code was changed during the gate. Harness scripts in the scratchpad were fixed and rerun; the failing runs are recorded above and are not counted as results.

Problems and notes:
- The phase1-proof.ps1 exit 1 (3 FAILs) is the blocker for this gate. The implementer's segment-aware re-check with the same kill rule and bounds passed, which supports the script-assumption explanation. That is the implementer's diagnosis, not a gate pass.
- The ad hoc and retry smoke harness bugs (segproof1, smoke1, smoke2) were corrected before the passing reruns.
- The stall case (2) of the retry smoke is incomplete in the record (see above).

NOT RUN:
- `cargo deny --locked check` and coverage (cargo-llvm-cov) are not in the gate command list, so not run. The 80% coverage target is therefore not verified for this wave.
- Ignored in the gate run and not run: full_5gib_segmented_exact_size_stream_compare, connect_timeout_15s_blackhole, iae_from_sta_writes_zone_identifier.
- plan_tests_missing for S1.12: the connect_timeout_15s_blackhole slow tier; tls_untrusted_cert_fatal and the https_to_http e2e over TLS (see the S1.12 section).

### S1.13 Admission, connection budgets, bandwidth caps, priorities (2026-11-15)

RED: `cargo test -p forgedm-core --locked --no-run --test admission --test rate_limit_e2e` failed to compile (exit 101): E0432 unresolved import `forgedm_core::engine::budgets` (admission.rs and tests/support/mod.rs); E0432 no `GlobalLimits` in `engine` (rate_limit_e2e.rs); E0425 cannot find function `start_with_space` in module `engine`. The source record for this slice is truncated partway through the description of the post-implementation run, so that run's output is not recorded here.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt -p forgedm-core -- --check` | 0 | clean |
| `cargo clippy -p forgedm-core --all-targets --locked -- -D warnings` | 0 | no warnings |
| `cargo test -p forgedm-core --locked` | 0 | all test binaries ok, 0 failed; 2 pre-existing ignored. The implementer reported a slice total of 304 passed, 0 failed, 2 ignored; the per-binary results were not re-summed |
| `git diff --exit-code Cargo.lock` | 0 | unchanged |

NOT RUN (plan_tests_missing): none reported for this slice.

Deviations:
- Admission does not skip hosts in cooldown. ARCHITECTURE 4.2 mentions it, but the S1.13 test list and the task bullets do not. Cooldown is still honoured per request through Resilience::gate.
- The disk-full precheck runs at stage() while the download is still probing. A.6 has no probing -> paused edge, so stage() moves the download to downloading (ProbeSucceeded) before returning PauseFor(DiskFull). The result is paused(disk_full), and a later Resume starts fresh; this replaces the earlier failed(disk_full) for that case. The 1 GiB recheck (stream_body and segmented check_space) uses the injectable FreeSpace reading. It is not exercised end to end because that needs more than 1 GiB written; only the A.9 constants and ensure_space are unit-tested.
- Only the global limits are stored in settings. The per-download rate limit lasts for the agent session, like the per-download connection cap. The global max_connections is stored as the default for new downloads; it is not a live cap on downloads already running. EnqueueUrl rate_limit_bps and start_paused are still refused as before.
- A connection permit is held for one request, per ARCHITECTURE 4.2. A worker also yields its permit mid-response (gate.should_yield) when another download's first connection is waiting and the yielding download holds at least two permits; without this a small download waited about 6 s behind a big one. The probe's permit is carried into the single-connection transfer that follows it, so a small download does not queue twice.
- Per-host AIMD is enforced with live host slots checked against HostHandle::limit() at the moment a slot is taken, plus Pool.rebalance retiring workers above the limit. It is not a tokio Semaphore that forgets permits. The global budget is a real tokio Semaphore.
- StoredJob gained priority, queue_position and created_at_ms. DownloadStore gained set_priority and ids_in_states, which default to refusing. Storage gained Downloads::set_priority. The existing StoredJob literals in tests were updated to match.
- To keep engine/mod.rs under 800 lines (it is now 768), Supervisor moved to engine/supervisor.rs and the retry-timer actor code to engine/retry_timer.rs. This is a pure move.
- crates/forgedm-agent/tests/process.rs: unimplemented_actions_return_not_implemented was replaced by every_action_of_the_contract_is_served, because PauseAll, ResumeAll, UpdateLimits and UpdatePriority are now served. The handlers/mod.rs test limits_that_belong_to_later_slices_are_refused_not_ignored was replaced by tests of the new behaviour. The implementer reports that no test was weakened.
- Extra tests beyond the plan list: pause_all_with_discard_progress_includes_non_resumable, global_speed_limit_bounds_segmented_transfers_too, rate_limit_of_an_unknown_download_is_not_found, plus unit tests in admission, budgets, hosts, limits_store and the CLI grammar.

Open (from the implementer and reviewers; see SECURITY_REVIEW "S1.13"):
- S1.13-rust-reviewer-1: a voluntary permit yield still counts against MAX_REQUESTS_PER_SEGMENT, and no test covers it. It needs a SegmentEnd::Yielded variant and a test.
- S1.13-rust-reviewer-2: a first connection still holds a global permit while it waits for a host slot. This is bounded and follows the documented order; it should be noted as accepted in ARCHITECTURE 4.2, which this slice does not own.
- S1.13-rust-reviewer-3: per-download token buckets are freed only on Remove.
- S1.13-rust-reviewer-4: a download-scope or global UpdateLimits request can be applied only in part.
- S1.13-code-reviewer-5: limit constants are still duplicated across the core, agent and protocol crates.

### W9 wave gate (2026-11-15)

RED: not applicable. This is the gate run over the workspace after S1.13; its RED notes are in the section above.

| Command | Exit | Result |
|---|---|---|
| `cargo fmt --all -- --check` (CARGO_TARGET_DIR unset, shared target dir) | 0 | no formatting differences |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | clean, no warnings; finished dev profile in 10.38 s |
| `cargo test --workspace --locked` (output kept in the session scratchpad as test.log) | 0 | 89 test binaries including doc-tests: 739 passed, 0 failed, 3 ignored (full_5gib_segmented_exact_size_stream_compare, connect_timeout_15s_blackhole, iae_from_sta_writes_zone_identifier) |
| `git diff --exit-code Cargo.lock` (checked after the tests and again at the end) | 0 | unchanged |
| `cargo build --workspace --bins --locked` | 0 | built forgedm.exe and forgedm-agent.exe in target\debug; forgedm-fixture.exe was already up to date |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1` (path quoted in the record) | 0 | 13 PASS, 0 FAIL, 0 NOT RUN: no-tls-bypass, no-assert-sql-safe, unsafe-only-in-platform, command-new-allowlist, layering, no-fixture-in-shipped, no-fault-injection-in-release, no-reqwest-compression, tauri-versions-match, no-loose-versions, lockfiles-committed, manifest-asinvoker, eslint-bans |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows-smoke.ps1` (path quoted in the record) | 0 | 22 PASS, 0 FAIL, 0 NOT RUN. Includes agent-has-no-tcp-listener, lock held and released, a second agent exits 3, peak working set 31.2 MB, and no-leftover-processes |
| limits smoke run 1, `powershell -File scratchpad\limits-smoke.ps1` (5 x 8 MiB, slow=2000000, --connections 1; logs kept in the scratchpad) | 0 | 13 PASS. Max active 3 across 341 samples, with 2 queued. All 5 completed with matching SHA-256. No forgedm process left. The limit check is not meaningful in this run: the unlimited baseline was only 1.86 MiB/s, below the 2 MiB/s limit (see Problems) |
| limits smoke run 2, `scratchpad\limits-smoke.ps1 -Connections 0 -UnlimitedSampleSec 1` (default 8 connections) | 0 | 14 PASS, 0 FAIL. Default limits were max_active=3, speed off, 8 connections. Unlimited baseline 2.99 MiB/s. Max active 3 across 296 `list --json` samples, with 2 queued. After `forgedm limits --speed 2097152` (exit 0), 9,961,472 B came in over 5.008 s (1.897 MiB/s), under the 11,026,878 B bound (rate x t plus 512 KiB burst), with 3 downloads still active. `pause-all` (exit 0) paused all 5 mid-download, and the byte sum stayed at 16,465,920 for 1.5 s. `resume-all` (exit 0) resumed all 5. All 5 completed at 8,388,608 B with SHA-256 equal to the fixture hash. `agent stop` exit 0, agent exit 0, fixture killed, 0 forgedm processes left, 0 WARN/ERROR lines in the agent log |
| limits smoke run 3, `scratchpad\limits-smoke.ps1 -Connections 0 -UnlimitedSampleSec 1.5 -LiftSec 2` | 1 | 14 PASS, 1 FAIL. Every required gate check passed (max active 3; limited window 2.002 MiB/s within the 11,001,365 B bound against a 3.38 MiB/s baseline; pause-all and resume-all exit 0; 5/5 SHA-256 match; no process left). The one failure was the extra check limit-lift-rate-rises, which the record attributes to the harness: the measurement window started after the `speed off` command returned, so about 6 MB had already drained and the window saw only 2 freshly started downloads. Superseded by run 4 |
| limits smoke run 4 (final), `scratchpad\limits-smoke.ps1 -Connections 0 -UnlimitedSampleSec 1.5 -LiftSec 2` (lift window starts at the snapshot before the command) | 0 | 15 PASS, 0 FAIL. Unlimited 3.48 MiB/s. Max active 3 across 399 samples, with 2 queued. Limited: 9,699,328 B over 4.987 s (1.855 MiB/s), under the 10,982,323 B bound, with 3 still active. After `limits --speed off` the rate rose to 4.62 MiB/s over 2.00 s. `pause-all` paused the 2 unfinished downloads (3 had already completed) and bytes stayed frozen. `resume-all` resumed 2. All 5 SHA-256 match. `agent stop` 0, agent exit 0, fixture killed, 0 forgedm processes |
| `tasklist \| grep -i forgedm` (after all smokes) | 1 | no match (grep exit 1): no forgedm process is running |

Gate all_green: true, as recorded by the coordinator. Limits smoke run 3 exited 1 and is recorded above as a FAIL; the lift check rests on run 4 and the limit check on runs 2 and 4.

Workspace tests: 739 passed, 0 failed, 3 ignored.

Problems and notes:
- The fixture's `slow=<Bps>` throttle is clamped to 64 KiB per 100 ms tick (tests/fixtures/src/body.rs: CHUNK 64 KiB, SLOW_TICK 100 ms), so slow=2000000 gives about 640 KiB/s per connection, not 2 MB/s. With --connections 1, three active downloads only reach about 1.86 MiB/s, below a 2 MiB/s limit, so run 1 proves nothing about the limit. The passing evidence comes from default connections (runs 2 and 4), where the unlimited baseline was 2.99 to 3.48 MiB/s.
- The speed limit governs how fast the agent accepts bytes. While it is in force, the fixture keeps filling the kernel TCP receive buffers, and they drain at once when the limit is lifted (about 6 MB within 0.11 s in run 3). This is expected for an application-level limiter and is not a defect.
- The limits smoke script is scratch, not part of the repo; its logs are kept alongside it in the scratchpad.

NOT RUN:
- Not in this gate's command list: cargo-llvm-cov coverage (no coverage figure is recorded for S1.13), cargo-deny, and the tauri build.
- Ignored in the gate run and not run: full_5gib_segmented_exact_size_stream_compare, connect_timeout_15s_blackhole, iae_from_sta_writes_zone_identifier.
- plan_tests_missing for S1.13: none reported.
