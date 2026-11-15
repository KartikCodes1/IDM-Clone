Status: ACCEPTED at GATE 1 (2026-11-15). Implementation is in progress; docs/IMPLEMENTATION_STATUS.md records what is actually built and tested.

# ForgeDM Architecture: Phase 0 and Phase 1

| Field | Value |
|---|---|
| Date | 2026-11-15 |
| Scope | Master spec section 13 Phase 0-1, section 14, section 12 engine tests. Execution prompt sections 1, 3, 4, 6, 7. Phase 2+ only as interfaces kept open (section 10). |
| Canonical names | `docs/PHASE_0_1_PLAN.md` Appendix A (protocol, states, error codes, exit codes, constants, names). This document gives design and rationale and does not redefine them. |
| Slices and tests | `docs/PHASE_0_1_PLAN.md` sections 4-5 (slice ids S0.x / S1.x, test names) |
| Threats and invariants | `docs/THREAT_MODEL.md` (Inv-01..Inv-25) |

**Conventions**
- **MUST** is normative; an unmet MUST is recorded Partial or Blocked in `IMPLEMENTATION_STATUS.md`.
- **UNVERIFIED** marks an unconfirmed claim; all are listed in section 12.
- Byte ranges are half-open `[start, end)` as `u64`; the only conversion to HTTP's closed `bytes=s-(e-1)` is `RangeRequest::header_value()`.
- Wall-clock times are INTEGER unix milliseconds (`*_ms`); rates and ETAs use `Instant`.
- `[tag]` refers to section 13.

---

## 1. Workspace and crates

Directory layout: PHASE_0_1_PLAN section 3. Repo root = Cargo workspace root (ADR-001).

### 1.1 Crates

| Package | Responsibility | Internal deps | `unsafe` |
|---|---|---|---|
| `forgedm-protocol` | Wire envelopes, payloads, DTOs, events; the shared value enums (`DownloadState`, `ErrorCode`, `DownloadErrorCode`, `Priority`); pure incremental frame codec; `validate()`; golden JSON. No I/O, no tokio. | none | forbidden |
| `forgedm-platform` | Safe Win32 wrappers: SID, pipe instances with DACL, peer identity, lock file, known folders, DPAPI, wide/verbatim paths, fs (sparse, file id, no-replace move, final path, volume info, free space), MOTW (Zone.Identifier, IAE). | none | **only crate allowed**; `// SAFETY:` on every block |
| `forgedm-core` | Domain and engine: URL policy, naming, state machine, HTTP semantics (ranges, validators, probe, redirect policy), retry, rate limit, progress, client, writer, checkpoint, resume, recovery, finalize, transfer, engine. Ports `DownloadStore`, `Clock`, `Sleeper`. | protocol (value enums only), platform | forbidden |
| `forgedm-storage` | sqlx SQLite pools, migrations, repositories, `SecretSealer` port. | protocol (value enums only) | forbidden |
| `forgedm-ipc` | Async frame transport, pipe server accept loop, client (busy retry, server verification, correlation, subscriptions), agent bootstrap. Shared by agent, CLI, desktop, future native host. | protocol, platform | forbidden |
| `forgedm-agent` | Lifetime, single instance, reconcile, dispatch, StateHub, `store_adapter` (storage -> core port), DPAPI sealer, `AgentError::to_wire()`, shutdown. bin + lib. | core, storage, ipc, protocol, platform | forbidden |
| `forgedm-cli` (bin `forgedm`) | `status`, `list`, `agent stop`, `add`, `watch`, `pause`, `resume`, `cancel`, `retry`, `remove`, `limits` | ipc, protocol | forbidden |
| `forgedm-desktop` | Tauri shell; ACL-gated commands over an `ipc::Client` | ipc, protocol | forbidden (justified local allow only if Tauri macros require it, S0.10) |
| `forgedm-fixture` | Adversarial HTTP server (axum + raw TCP), deterministic content, request log. `publish = false`. | none | forbidden |

### 1.2 Dependency rules (checked by `check.ps1 :: layering`)
- `protocol` and `platform` depend on no internal crate.
- `core` and `storage` never depend on each other, on `ipc`, or on `agent`. They use only protocol's value enums, never envelopes or the codec (ADR-003).
- `cli` and `desktop` never depend on `core` or `storage`: only the agent owns the DB and the engine.
- No crate takes a non-dev dependency on `forgedm-fixture`.

### 1.3 Module map (unit of file ownership)
```
protocol/src: lib frame envelope actions results events state errors validate version ; tests/golden/v1/*.json
platform/src: error wide sid pipe peer instance_lock known_folders dpapi fs motw
core/src:     lib error ports url_policy state naming/{mod,content_disposition,sanitize}
              http/{mod,content_range,validators,range_decision,probe_classify,redirect}
              clock retry rate_limit progress client probe writer checkpoint resume recovery finalize hash
              transfer/{mod,single,segmented,segment_table,worker} engine/{mod,task,admission,budgets,hosts}
              testing (feature test-support)
storage/src:  lib error open migrate sealer repo/{downloads,segments,events,settings} ; migrations/0001_init.sql ; build.rs
ipc/src:      lib error server client identity subscribe bootstrap
agent/src:    main lib error config logging app sealer events_hub engine_host store_adapter fault (feature fault-injection)
              handlers/{mod,lifecycle,query,enqueue,control,limits}
cli/src:      main cli client output commands/{status,list,agent,add,watch,control,limits}
fixtures/src: lib content server scenarios raw_tcp request_log verify ; bin forgedm-fixture
desktop:      src-tauri/src/{main,lib,commands,error,agent_client}.rs ; src/{main.tsx,App.tsx,index.css,lib/ipc.ts,test/setup.ts}
```

### 1.4 Toolchain, lints, profiles
- **Rust:** stable 1.100.0 pinned in `rust-toolchain.toml` (D2); `rust-version = "1.94"` (sqlx 0.9.0) [sqlx-cl]; edition 2024; resolver 3.
- **Lints:** `unsafe_code = "deny"`, `unsafe_op_in_unsafe_fn = "deny"` (platform FFI modules opt in locally; every other crate adds `#![forbid(unsafe_code)]`, except `forgedm-desktop` if S0.10 shows Tauri macros need a justified local allow). Clippy: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `print_stdout`, `print_stderr`, `await_holding_lock`, `undocumented_unsafe_blocks`, `cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap` (errors under `-D warnings`). Tests may unwrap/expect; the CLI may print.
- **`clippy.toml` disallowed-methods:** full list in PHASE_0_1_PLAN S0.1 (TLS bypass old and new names, custom roots, key logging, preconfigured TLS, early data, total timeouts, `Client::new`, `std::fs::rename`, `tokio::fs::rename`, `OpenOptions::append`).
- **Profiles:** dev deps `opt-level = 2` (sha2 3); release `lto = "thin"`, `codegen-units = 1`, `panic = "unwind"`, `overflow-checks = true`, `debug = "line-tables-only"`.
- **Build env:** `.cargo/config.toml` `[env] AWS_LC_SYS_PREBUILT_NASM = "1"` (D1). The repo path contains a space; S0.1 verifies build scripts tolerate it (fallback: `CARGO_TARGET_DIR` without spaces).

### 1.5 Research-verified versions (2026-11-15)

Latest stable, non-yanked, non-prerelease unless noted. Sources: section 13.

| Item | Version | Published | Features / notes | Source |
|---|---|---|---|---|
| rustc stable | 1.100.0 | 2026-11-09 (manifest 2026-11-12) | pin via `rust-toolchain.toml`; rustup had no default at inspection | [rs-channel] |
| MSRV | 1.94 | | set by sqlx 0.9.0 (tauri 1.90, sha2/proptest 1.88, uuid 1.89) | [sqlx-cl], [crates-api] |
| tokio | 1.54.1 | 2026-10-21 | per crate: rt-multi-thread, macros, net, fs, io-util, sync, time, signal (1.54.0 yanked) | [crates-api], [tokio-pipe] |
| tokio-util | 0.7.20 | 2026-10-09 | `rt` (TaskTracker); CancellationToken needs no feature | [crates-api] |
| reqwest | 0.13.5 | 2026-09-08 | `default-features = false, ["rustls", "system-proxy"]`; no `http2`, no compression, no `stream` (`Response::chunk()`); rustls + aws-lc-rs + platform verifier | [rq-features], [rq-src], [rq-changelog] |
| aws-lc-rs / aws-lc-sys | 1.19.0 / 0.47.0 | 2026-10-28 | transitive; MSVC needs NASM or `AWS_LC_SYS_PREBUILT_NASM=1` | [aws-lc-win] |
| sqlx | 0.9.0 | 2026-05-21 | `default-features = false, ["runtime-tokio", "sqlite-bundled", "macros", "migrate"]`; queries take `impl SqlSafeStr`; MSRV 1.94 | [sqlx-cl], [sqlx-src] |
| libsqlite3-sys | 0.37.0 | | max allowed by sqlx 0.9.0 (`<0.38`); bundled SQLite version UNVERIFIED (runtime check >= 3.37) | [sqlx-src] |
| serde / serde_json | 1.0.229 / 1.0.154 | 2026-07-18 / 2026-10-11 | `derive` | [crates-api] |
| thiserror / anyhow | 2.0.21 / 1.0.104 | 2026-09-23 / 2026-07-18 | anyhow only in bin `main.rs` | [crates-api] |
| tracing / tracing-subscriber / tracing-appender | 0.1.44 / 0.3.23 / 0.2.5 | 2025-12-18 / 2026-03-13 / 2026-04-17 | env-filter, fmt, json; `rolling::Builder::max_log_files` exists (verified) | [crates-api] |
| clap | 4.6.7 | 2026-09-14 | `derive` | [crates-api] |
| sha2 / hex | 0.11.1 / 0.4.3 | 2026-10-12 / 2021-03-03 | sha2 0.11 output has no `LowerHex`: use `hex::encode(out.as_slice())` | [crates-api], [hybrid-array] |
| uuid | 1.29.2 | 2026-11-09 | `v7`, `serde` | [crates-api] |
| url / httpdate / bytes | 2.5.8 / 1.0.3 / 1.12.1 | 2026-01-05 / 2023-08-13 / 2026-07-08 | | [crates-api] |
| windows | =0.62.2 | 2025-10-06 | latest published; master is 0.100.0 (unpublished, new module paths) so pin exactly. Features: Win32_Foundation, Win32_Security, Win32_Security_Authorization, Win32_Security_Cryptography, Win32_System_Pipes, Win32_System_Threading, Win32_Storage_FileSystem, Win32_System_IO, Win32_System_Ioctl, Win32_System_Com, Win32_UI_Shell | [win-src], [win-master] |
| windows-sys | 0.61.2 | 2025-10-06 | transitive only (tokio); never direct | [crates-api] |
| tauri / tauri-build | 2.12.5 / 2.7.4 | 2026-10-21 | MSRV 1.90; depends on windows ^0.62; 3.0.0-alpha.6 exists, avoid | [tauri-crates] |
| axum / hyper / hyper-util / http-body-util / http | 0.8.9 / 1.12.0 / 0.1.21 / 0.1.5 / 1.5.0 | | dev only (fixture); hyper `server`, `http1` | [crates-api] |
| futures-util / tempfile / proptest | 0.3.34 / 3.27.0 / 1.12.0 | | dev only | [crates-api] |
| rcgen / tokio-rustls | 0.14.10 / 0.26.6 | | D9 only; tokio-rustls already transitive via reqwest `rustls` | [crates-api] |
| Node.js | 26.13.1 LTS | Active LTS 2026-10-28 | D3; installed 25.5.0 is EOL (2026-06-01) | [node-sched] |
| pnpm | 10.29.2 | installed | `packageManager` pinned to it | [npm] |
| @tauri-apps/cli / api | 2.12.4 / 2.12.4 | 2026-10-21 / 2026-10-14 | major.minor must match the tauri crate | [npm] |
| react / react-dom / @types/react(-dom) | 19.3.1 / 19.3.1 / 19.3.0 | 2026-10-14 | | [npm] |
| vite / @vitejs/plugin-react | 8.3.8 / 6.3.0 | 2026-11-11 | Rolldown-based | [npm] |
| typescript | ~6.0.3 | | not 7.0.2 (typescript-eslint 8.72.2 peer `<6.1.0`) | [npm] |
| tailwindcss / @tailwindcss/vite | 4.3.5 | 2026-11-06 | CSS-first, no config file | [npm] |
| vitest / @vitest/coverage-v8 / jsdom | 5.1.0 / 5.1.0 / 31.0.1 | | exclude Node 25; fallback 4.1.12 / 4.1.12 / 29.1.1 | [npm] |
| @testing-library react / dom / jest-dom / user-event | 16.3.3 / 10.4.2 / 7.0.1 / 14.6.10 | | dom is a required peer | [npm] |
| eslint / @eslint/js / typescript-eslint | 10.15.0 / 10.0.1 / 8.72.2 | 2026-11-13 (eslint) | | [npm] |
| eslint-plugin-react-hooks / -react-refresh / globals | 7.1.1 / 0.5.7 / 17.14.0 | | | [npm] |
| @types/node | 26.6.6 | | match the chosen Node major (24.19.2 for Node 24) | [npm] |

**Rejected:** rusqlite (`links = "sqlite3"` conflict with sqlx's `<0.38`), governor 0.10.4 (ADR-013), windows-core 0.100.0 direct (second copy), interprocess 2.4.8, rand 0.10.3 (splitmix64 jitter instead), time/chrono (unix ms), backon / reqwest-retry (ADR-014), ts-rs 12.0.1 (deferred; defaults u64 to `bigint`), trauma / async-fetcher / gosh-dl / bolt-load / bytehaul (design references only; no code copied).

---

## 2. Process model and lifecycle

### 2.1 Processes
- **Agent** `forgedm-agent.exe`: one per user (per data root), long-lived; owns runtime, engine, DB and all disk writes. Windows subsystem in release, console in debug.
- **Clients:** `forgedm.exe` (per invocation), `forgedm-desktop.exe`, Phase 3 native host (stdout reserved for framing). Clients never touch the DB or download files.

### 2.2 Paths
Resolved with `SHGetKnownFolderPath`, never environment variables (spoofable).
```
%LOCALAPPDATA%\ForgeDM\  forgedm.db (+ -wal, -shm)   logs\agent.log.YYYY-MM-DD (keep 14)   run\agent.lock
```
- Default download directory: `FOLDERID_Downloads`, resolved at each enqueue, canonicalised. It never defaults under `%LOCALAPPDATA%\ForgeDM` or Tauri's `%LOCALAPPDATA%\<identifier>` (the NSIS "delete app data" option removes the latter).
- `--data-dir <abs>` (dev/tests) replaces the data root and derives a distinct pipe name (section 2.4, ADR-018).

### 2.3 Agent startup (order matters)
1. Parse args (exit 2). Install panic hook.
2. Resolve the user SID and data root; create it; reject a name-surrogate reparse point under it (exit 6).
3. Take `run\agent.lock` with `File::try_lock()`, retrying up to 3 s (lock release after a kill can lag) -> else exit 3.
4. Start rolling file logs.
5. Create the first pipe instance with `first_pipe_instance(true)` to claim the name early, retrying `NameInUse` up to 3 s -> else `forgedm::security` event and exit 4. No accept loop yet; early clients wait inside their request timeout.
6. Open the DB, assert `sqlite_version() >= 3.37` (STRICT tables), migrate (exit 5 on any failure, incl. DB newer than the binary).
7. Reconcile (section 6.3).
8. Start engine and StateHub; start the accept loop; emit `AgentStatus{ready}`.

### 2.4 Single instance and pipe name (ADR-006)
- **Authoritative gate:** exclusive lock on `run\agent.lock` (LockFileEx; user-only profile ACL; released by the OS on crash; covers all sessions of the user). Named mutexes are not used: squattable and per session [ms-mutex].
- **Pipe ownership:** if the agent holds the lock but `first_pipe_instance` fails, the name is squatted: security alert, never join the existing pipe.
- **Name:** `\\.\pipe\ForgeDM-<SID>`; a non-default data root appends `-<first 16 hex of SHA-256(lowercased canonical root, UTF-16LE)>` so isolated test agents run in parallel without any test-only bypass.
- The pipe namespace is machine-wide (verified), so a second session of the same user reaches the same agent, which is intended.

### 2.5 Client bootstrap (`ipc::bootstrap`, ADR-005)
1. Open the pipe. `ERROR_PIPE_BUSY` (231): retry every 50 ms up to 5 s. `NotFound` with spawning allowed: step 2.
2. Spawn only the caller-supplied `expected_agent_path` (production: canonical sibling `forgedm-agent.exe` of `current_exe()`; tests: `CARGO_BIN_EXE_forgedm-agent`). No PATH search, no env-var override. Flags `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`, null stdio, proxy env vars removed (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, both cases) so routing does not depend on which client started the agent. Poll up to 10 s; child exit 3 means another agent won: reconnect.
3. Verify the server (section 3.5) before sending any byte; then Handshake.
- `status` never auto-starts; `add` does (`--no-start` opts out). `agent start` is not in Phase 0/1 scope (PLAN S0.9, S1.7 list no such command). An agent spawned inside a job with KILL_ON_JOB_CLOSE dies with the job (UNVERIFIED; `CREATE_BREAKAWAY_FROM_JOB` not used).

### 2.6 Shutdown
Triggered by `ShutdownAgentSafely` or Ctrl+C / close in console mode.
1. Stop admitting work; new mutating requests get `shutting_down`; broadcast `AgentStatus{shutting_down}`.
2. Stop every active download with reason Shutdown: workers stop and release claims, writer drains and flushes, final checkpoint. `probing`, `downloading` and `verifying` -> `interrupted` (reason `agent_shutdown`); `pausing` -> `paused`; `finalizing` has no A.6 edge out of shutdown, so it is left for reconcile (6.3); `retry_waiting` and `paused` unchanged.
3. `TaskTracker` waits up to 15 s (implementation constant, not an A.9 tunable); anything still running is reconciled as a crash next start.
4. Close pools, release the lock, exit 0.
- Logoff: a windowless process gets no notification; Phase 1 relies on 2 s checkpoints plus reconcile. Phase 2 adds a hidden window handling `WM_QUERYENDSESSION` (UNVERIFIED).

### 2.7 Flags
`--data-dir <abs>`, `--proxy system|none` (default `system`; tests use `none`; the effective proxy source is reported in `GetAgentStatus.proxy_source`), `--log-level <filter>` (default `info`). Exit codes: PHASE_0_1_PLAN A.8.

---

## 3. IPC

### 3.1 Contract
Framing, envelopes, versioning, actions, DTOs, events, error codes: PHASE_0_1_PLAN Appendix A (ADR-004). Two-step strict parse on the server; tolerant clients. Golden files in `protocol/tests/golden/v1/` are round-tripped by Rust and validated by the desktop's handwritten TS guards. ts-rs is deferred (its default maps u64 to `bigint`, which JSON.parse never produces).

### 3.2 Server (`ipc::server`, pipe creation in `platform::pipe`)
- **Capacity invariant (ADR-005):** `PIPE_MAX_INSTANCES = MAX_ACTIVE_CONNECTIONS + 1 = 17` (tokio panics at >= 255). The accept loop:
  1. acquires one of 16 semaphore permits;
  2. awaits `connect()` on the current listening instance;
  3. creates the replacement listening instance (`first_pipe_instance(false)`, same DACL);
  4. only then hands the connected instance to its connection task.
  A connected handle is never dropped before its replacement exists, so the instance count never reaches 0 and a squatter cannot recreate the name mid-run. A 17th client connects to the listening instance and is served when a permit frees; further clients get `ERROR_PIPE_BUSY` and retry. Any instance-creation failure after startup is fatal (security event, server stops, agent exits 4); it is never retried without `first_pipe_instance(true)`.
- Per connection: caller check (section 3.4) before reading a frame; Handshake within 5 s; frame body within 10 s of its header; idle timeout 60 s unless subscribed; one request at a time.

### 3.3 Pipe DACL and creation
```
SDDL = "D:P(D;;GA;;;NU)(A;;GA;;;<user-SID>)S:(ML;;NWNR;;;ME)"
```
- Protected DACL; NETWORK deny first (ACE order is preserved and evaluation stops at the first match); only the current user allowed (SYSTEM omitted); medium label with no-write-up and no-read-up. Verified non-admin on this machine: readback `D:P(D;;FA;;;NU)(A;;FA;;;<sid>)S:AI(ML;;NWNR;;;ME)`; same-user client connected [r-win32]. A NULL descriptor would grant Everyone read access and is forbidden.
- SID string validated against `^S-1-\d+(-\d+)+$` before formatting into SDDL.
- Calls (windows 0.62.2): `OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY)`; `GetTokenInformation(TokenUser)` twice into an 8-byte-aligned `Vec<u64>`; `ConvertSidToStringSidW` (LocalFree via `Owned<HLOCAL>`); `ConvertStringSecurityDescriptorToSecurityDescriptorW(.., SDDL_REVISION_1, ..)` with the `Owned<HLOCAL>` kept alive for the server lifetime; tokio `ServerOptions::new().first_pipe_instance(first).reject_remote_clients(true).pipe_mode(PipeMode::Byte).max_instances(17).in_buffer_size(65536).out_buffer_size(65536)` then `unsafe create_with_security_attributes_raw(&name, &raw mut sa)` with `SECURITY_ATTRIBUTES{bInheritHandle: false}`. `PermissionDenied` -> `NameInUse`. Remote rejection is per instance, so it is set on every instance.

### 3.4 Caller check (every connection)
1. `GetNamedPipeClientProcessId(HANDLE(h.as_raw_handle()), &mut pid)`.
2. `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)`; any failure rejects (fail closed).
3. `OpenProcessToken` -> `GetTokenInformation(TokenUser)`; accept only if `EqualSid(peer, me).is_ok()` (in windows 0.62 "not equal" is an `Err` with code 0).
4. `QueryFullProcessImageNameW(PROCESS_NAME_WIN32)` recorded for logs and the Phase 3 role binding.
- The DACL is the primary control; PID reuse between connect and check is residual. Impersonation is not used in Phase 1.

### 3.5 Client verification (anti-squatting, before the first write)
1. `ClientOptions::new().open(name)` keeping the default `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` (any non-tokio client must set these itself; std leaves them unset).
2. `GetNamedPipeServerProcessId` on the client handle (verified to work).
3. Server token SID must equal the current user's; the server's full image path, canonicalised, must equal the explicit `expected_agent_path` case-insensitively. Any failure: abort, send nothing (CLI exit 14).

### 3.6 StateHub, sequencing and replay (ADR-019)
- Single actor in `agent::events_hub` owning: summary map (non-terminal downloads + 1000 most recent), `seq` counter, ring buffer of the last 1024 events, subscriber list.
- Engine events arrive on a bounded channel (1024); the hub persists nothing itself: it assigns `seq` only to changes the engine already committed.
- `GetSnapshot` returns summaries plus the current `seq` S. `Subscribe{agent_instance_id, after_seq: S}` registers the subscriber and replays `(S, now]` in the same actor turn, so no event between snapshot and subscribe is lost. Evicted `after_seq` or a different instance id -> `resync_required`.
- Per-connection outbound queue 256 frames; events use `try_send`; overflow drops the subscription and sends `ResyncRequired{lagged}`; responses use `send().await` (backpressure per connection).

### 3.7 Desktop shell
Tauri commands map to IPC actions and return protocol types; commands are ACL-gated through `tauri_build::AppManifest::new().commands(&[..])` because app commands are otherwise not ACL-checked [tauri-acl]. The webview never speaks the pipe protocol. CSP (PHASE_0_1_PLAN S0.10) is verified only on `tauri build --debug --no-bundle`, because Tauri's CSP is not applied under `tauri dev` with a Vite devUrl on desktop [tauri-csp] (and the ECC hook blocks `tauri dev`). ADR-022.

---

## 4. Engine (`forgedm-core`)

### 4.1 Components
```
dispatch --mpsc(64)+oneshot--> Engine actor (admission, budgets, per-download handles)
   -> DownloadTask (1 tokio task per admitted download; sole mutator of its state)
        probe -> plan -> JoinSet<Worker> (child CancellationTokens)
           Worker --bounded(16) WriteCmd{Data, Flush, Close}--> Writer std::thread (owns the File)
        checkpoint loop (every 2 s): Flush -> frontiers -> DownloadStore::save_checkpoint
        progress tick 250 ms -> EngineEvent (try_send) ; verify/finalize on a blocking thread
EngineEvent --mpsc(1024)--> StateHub (agent)
```
- The dispatch channel `mpsc(64)` is an implementation constant, not an A.9 tunable; a full dispatch channel maps to `busy` (A.7). The engine-to-hub channel is ENGINE_TO_HUB (A.9).
- `DownloadStore` (core port) is implemented by `agent::store_adapter` over `forgedm-storage`; core tests use the in-memory fake in `core::testing`.
- Non-admitted downloads are mutated only by the Engine actor; every transition is a DB compare-and-set.
- Worst-case buffered memory about 6 MiB per active download (16 x 256 KiB writer queue + up to 8 worker buffers of 256 KiB), independent of file size.

### 4.2 Scheduler and budgets
- **Admission:** while `active < max_active_downloads`, pick the next eligible download, skipping hosts in cooldown. A due `retry_waiting` download first transitions to `queued` through the A.6 timer edge (`retry_waiting -> queued`; there is no `retry_waiting -> probing` edge). Admission then orders queued downloads by priority, `queue_position`, `created_at_ms` (PLAN S1.13 `priority_then_fifo_admission`, `due_retry_requeued_before_admission`). Active = probing, downloading, pausing.
- **Semaphores:** global then per-host, always in that order. The first worker awaits a permit; extra workers `try_acquire` and retry each second, so one download never starves another's first connection. Permits are held for the request lifetime.
- **AIMD per host** (`scheme://host:port`): 429, 503, or refused at open halves the host limit (min 1) by forgetting permits as they return and sets `cooldown_until`, pausing every segment to that host (no storm). +1 after 2 min clean, up to the configured max.
- **Runtime connection changes:** `UpdateLimits` reaches the task via `watch`; decreases cancel the youngest workers, which release claims (no progress lost).

### 4.3 HTTP client (ADR-007)
```rust
Client::builder().user_agent(concat!("ForgeDM/", env!("CARGO_PKG_VERSION")))
  .http1_only().no_gzip().no_brotli().no_deflate().no_zstd()   // guards vs feature unification
  .redirect(redirect::Policy::none()).referer(false)
  .connect_timeout(15s).read_timeout(30s)                       // never .timeout(): total deadline
  .pool_idle_timeout(90s).pool_max_idle_per_host(16).http1_max_headers(128)
  .tls_backend_rustls() /* ProxyMode::None => .no_proxy() */ .build()?   // never Client::new() (panics)
```
- `pool_idle_timeout` (90 s) and `pool_max_idle_per_host` (16) are implementation constants, not A.9 tunables.
- Every request sends `Accept: */*`, `Accept-Encoding: identity`; `Range` and a strong `If-Range` where applicable. No Referer in Phase 1.
- There is no API to disable or alter TLS verification (clippy + `no-tls-bypass`). `resolve()` overrides exist only behind `test-support`.
- reqwest reads proxy env vars unless `.no_proxy()`/`.proxy()` is used; with `system-proxy` it also reads HKCU Internet Settings; PAC/WPAD unsupported [rq-proxy].

### 4.4 Probe (ADR-009)
1. Validate the URL (section 4.15).
2. **HEAD** (5 s), redirects followed manually. 2xx gives hints (Content-Length unless content-coded, ETag, Last-Modified, Date, Content-Disposition, Content-Type, Accept-Ranges, final URL). Errors, timeouts or 405 make HEAD inconclusive; probing continues.
3. **GET `Range: bytes=0-0`** with identity encoding, to the HEAD final URL or the original URL:
   - 206: single part, Content-Range exactly `bytes 0-0/T` or `bytes 0-0/*`, identity, body exactly 1 byte (read at most 2) -> range-capable.
   - 200: range ignored; dropped unread (the file is never fully downloaded as a probe).
   - 416 with `bytes */0`: zero-length; other 416 -> range ignored.
   - 401/407 -> needs credentials; 403/404/410 -> failed (or `needs_url_refresh` when re-probing a download with progress); 408/429/5xx per section 4.10.
4. GET wins over HEAD on conflict (logged as an event). Validators come from the GET.
5. Name: user-supplied, else Content-Disposition `filename*`, else `filename`, else last path segment of the final URL, else `download`; then sanitise (section 4.14).
6. Cost: at most 2 requests plus redirect hops.

### 4.5 Capability decision

| Probe result | transfer_mode | Connections | resume | Completion proof |
|---|---|---|---|---|
| 206 valid, T known, identity, T >= 8 MiB | `segmented` | up to max | `reliable` with a strong validator, else `best_effort` | length = T, SHA-256 recorded |
| 206 valid, T < 8 MiB | `single_ranged` | 1 | same | length = T |
| 206 with `/*` | `single_ranged_unknown_length` | 1 | `best_effort` | `length_verified = false` unless a checksum was supplied |
| 200 with Content-Length, identity | `single_nonresumable` | 1 | `not_resumable` (restart from 0) | length = Content-Length |
| 200 without Content-Length | `single_unknown_length` | 1 | `not_resumable` | `length_verified = false` (RFC 9112 section 8) |
| 416 `*/0`, or 200 with Content-Length 0 | `zero_length` | 0 | n/a | empty file |
| Non-identity Content-Encoding (200 or 206) | none | 0 | n/a | `failed(unsupported_content_encoding)` (Inv-07) |

### 4.6 Response validation before any write
Request `bytes=s-(e-1)`, `If-Range` only with a strong validator. Implemented by `http::range_decision::decide` over plain `ResponseMeta`.

| Response | Action |
|---|---|
| 206, single part, `Content-Range: bytes s-x/T`, `x <= e-1`, T equals stored (or `*` while unknown), identity, Content-Length (if present) = x-s+1, validators equal | **Accept.** Write only `[s, x]`; bytes beyond x are a violation. `x < e-1` is a short 206: the segment continues later. |
| 206 with wrong start, `x >= e`, different T, bad syntax (inverted, `T <= last`, unknown unit, overflow, > 20 digits), multipart | `range_protocol_violation`, nothing written; one re-probe, then fail |
| 206 or 200 with a non-identity Content-Encoding | `failed(unsupported_content_encoding)`; nothing written; no re-probe; never retried (A.7, Inv-07). Probe and transfer classify it the same way. |
| 200 to a ranged request | **Never written at offset s.** If `If-Range` was sent or validators/length differ: `remote_changed`, stop all workers, fail. Equal validators: re-plan as `single_nonresumable` only if nothing is committed, else `range_protocol_violation{reason: ranges_no_longer_supported}`. Exception: segment 0 with s = 0, equal validators and Content-Length = T may read up to e. |
| 416 | `bytes */T` with `s == T ==` stored total and equal validators: segment already complete (common after a crash between final flush and DB update). Different T: `remote_changed`. No Content-Range: re-probe. |
| 304, 412 | protocol violation |

### 4.7 Validators and resume identity (ADR-021)
- ETag parsed as `[W/]"opaque"`; only strong ETags are sent in `If-Range`.
- Last-Modified is strong only if `Date >= LM + 1 s`; sent byte-identical and only when there is no ETag.
- Resume identity: equal T plus equal strong ETag (or, without one, strong Last-Modified).
- No strong validator (`sample_verify`, S1.14): pick the segment with the most committed data beyond its start (>= 64 KiB); request `bytes=(c-65536)-(c-1)`; compare with disk; mismatch -> `remote_changed`; nothing with 64 KiB committed -> restart from zero. UI shows `best_effort`. Data is never appended blindly. (S1.9 interim: restart from zero.)
- Every resume re-probes the **original** URL (redirects re-followed) before writing.

### 4.8 Segment table (ADR-010)
One `std::sync::Mutex<SegmentTable>` per download; never held across `.await`; gives atomic checkpoint snapshots and splits.
```rust
struct Seg { no: u32, start: u64, end: Option<u64>, claim: u64, sent: u64, written: u64, flushed: u64, committed: u64,
             owner: Option<WorkerId>, retries: u32 }
// start <= committed <= flushed <= written <= sent <= claim <= end ; segments disjoint, cover [0, T) ; <= 256 segments
```
- **Claim a chunk** of n bytes at stream position p: lock; assert `p == claim` (mismatch aborts the worker as a bug); `allowed = min(n, end - claim)`; `claim += allowed`; unlock. Buffer allowed bytes; at 256 KiB send `WriteCmd::Data` and advance `sent`. If `allowed < n` or `claim == end`, flush the buffer and drop the response (closes the HTTP/1.1 connection).
- `written` advances when the writer completes a positional write; `flushed` from the `FlushReport` after `sync_data`; `committed` after the DB commit.
- **Worker exit** (error, cancel, retire): flush its buffer if the writer is healthy, then `claim = sent`, `owner = None`; claimed-but-unsent bytes are re-requested, never lost or duplicated.
- **Assign work:** unowned segment with work left and the lowest start; else split the victim with the largest `(end - claim) / ewma_speed` having >= 8 MiB remaining: `mid = align_up(claim + rem/2, 64 KiB)` with >= 4 MiB each side; create `[mid, end)` and set `victim.end = mid` under the same lock (so `mid >= claim` and no overlap); else the worker exits.
- **Plan:** `min(4, max_connections, ceil(T/MIN_SPLIT))` equal segments aligned to 1 MiB, starts staggered 150 ms. Adjacent fully committed segments merge at checkpoint. Progress counts accepted bytes (sum of `sent`); discarded bytes count as `wasted`. Endgame racing deferred.

### 4.9 Redirects and header scoping (ADR-008)
Followed manually (`Policy::none()`) through the pure `http::redirect::next_hop`:
- 301/302/303/307/308 with `Location` <= 8 KiB resolved relative to the current URL; http/https only; **https -> http blocked** (`redirect_downgrade`); at most 10 hops; visited set catches loops.
- **Private destinations:** a hop whose parsed host classifies as loopback, private, link-local, CGNAT, unique-local, unspecified, `localhost` or `*.localhost` is blocked (`redirect_blocked`) unless the original URL was itself local. DNS-name classification with a pinned resolver is Phase 3 (THREAT_MODEL NET-06).
- Each hop is logged as a redacted event; the final URL is sealed.
- **Header classes:** engine-owned (Host, Range, If-Range, Accept-Encoding, Content-Length, Transfer-Encoding, Connection, TE, Upgrade, Keep-Alive, Expect, Proxy-*) are never client-supplied. Origin-bound (Authorization, Cookie, all future custom headers) go only to the original origin: reqwest strips only five standard headers and compares only to the previous hop [rq-redirect]. Phase 1 sends none of them; the scoping decision is unit-tested for later phases.
- **Expired signed URLs:** 403/404/410 from the cached final URL during transfer trigger one re-resolve from the original URL plus an identity check, else `needs_url_refresh`; max 3 re-resolves per hour. 401/407 never take this path: they always go to `needs_credentials` after one request (section 4.10).

### 4.10 Retry and classification (ADR-014)
Full jitter `rand(0, min(60 s, 1 s * 2^n))` [aws-backoff] with injectable `Clock`, `Sleeper`, seeded jitter. Retry-After (seconds or HTTP-date computed against the response `Date`) honoured on 429, 503 and 3xx: <= 120 s inline; <= 1 h `retry_waiting`; longer `failed(rate_limited)`. Classification walks `source()` (`is_timeout`, `is_connect`, `is_dns`, `is_body`, `io::ErrorKind`, raw OS codes), never strings.

| Condition | Disposition |
|---|---|
| connect timeout/refused (limited), reset, read stall, premature EOF, temporary DNS (limited), 408, 500, 502, 504 | retry the segment from its current claim |
| 429, 503 | host cooldown + AIMD; honour Retry-After |
| 401, 407 | `needs_credentials` (`auth_required`) |
| 403, 404, 410 | probe: fail; transfer: one re-resolve, then `needs_url_refresh` |
| 400, 405, 411, 414, 451, unexpected 416, other 4xx, malformed framing, redirect errors | fail |
| TLS/certificate errors | fail (`tls_failure`); never retried or bypassed |
| disk full; device gone | `paused` (`disk_full` / `destination_unavailable`) |
| other local I/O errors | fail (`disk_io`) |

Budgets per PHASE_0_1_PLAN A.9. reqwest's built-in protocol-NACK retry is kept; retry middleware is not used because retries must restart from the claim.

### 4.11 Rate limiting (ADR-013)
Custom token bucket on `tokio::time`: `{rate: AtomicU64 (0 = unlimited), state: Mutex<{tokens, last}>}`, burst `max(250 ms of rate, 64 KiB)`. After each chunk a worker acquires from the per-download bucket, then the global bucket, in pieces <= 64 KiB (A.9 acquire chunk); TCP backpressure slows the sender. Limits change live. Tests use `tokio::time::pause()` (governor ignores paused time and has an immutable quota). Fairness is approximate (C21).

### 4.12 Progress, speed, ETA
- Counters: `accepted` (progress), `committed` (durable; after restart accepted = committed), `wasted` and `raw_received` (diagnostics).
- Speed sampled every 250 ms, `alpha = 1 - exp(-dt/tau)`: display tau 3 s; ETA tau 12 s double-smoothed; startup bias normalised by `1 - exp(-elapsed/tau)`; stalls decay toward 0 [indicatif].
- ETA only with known total, >= 2 s elapsed and >= 1 KiB/s, else null ("Estimating..."); changes only beyond 5% or 2 s; under a rate limit uses min(speed, limit). Unknown totals never show a percentage.
- Per-worker EWMA (tau 2.5 s) feeds split decisions and per-segment speeds.

### 4.13 Cancellation and backpressure
Token tree engine -> download -> worker with reasons Pause, Cancel, Shutdown, Remove, RestartRequired. Stop sequence: workers stop and release claims -> writer flushes -> checkpoint -> state transition. Every channel is bounded. A task panic surfaces as `JoinError::is_panic` -> `failed(internal)`; the agent keeps running (`panic = "unwind"`).

### 4.14 Filename sanitiser (`core::naming`)
1. `filename*` (RFC 5987) over `filename` (RFC 6266); header input capped at 4 KiB (ignored above).
2. Keep only the final segment after splitting on `/` and `\`.
3. Remove `<>:"/\|?*`, U+0000-U+001F, U+007F, and format/bidi controls U+200B-U+200F, U+202A-U+202E, U+2066-U+2069, U+FEFF. Removing `:` blocks ADS syntax and drive-relative paths.
4. Trim leading spaces and trailing dots/spaces (Win32 strips them silently, verified).
5. Prefix `_` when the stem (before the first `.`, trailing spaces trimmed, case-insensitive) is CON, PRN, AUX, NUL, CONIN$, CONOUT$, COM0-COM9, LPT0-LPT9, or COM/LPT followed by superscript 1, 2 or 3 (U+00B9, U+00B2, U+00B3). Deliberately broader than build 26300, where only bare NUL maps to a device.
6. Empty, `.` or `..` -> `download`.
7. Cap at 233 UTF-16 units (so `<name>.<id8>.forgedm.part` <= 255): truncate the stem, keep an extension of up to 16 units, never split a surrogate pair.
8. Double extensions are kept (MOTW plus a Phase 2 UI warning). Property tests: idempotent, non-empty, no forbidden char, not reserved, no trailing dot/space, within length.

### 4.15 URL handling (`core::url_policy`)
- Parsed with `url`; http/https only; host required; userinfo rejected (credentials are a separate, deferred field); <= 8 KiB; control chars rejected; fragment stripped.
- `classify_host(&url::Host)` works on the parsed host enum, never strings, so WHATWG-normalised forms (`2130706433`, `0x7f.1`, `127.1`) and `[::ffff:127.0.0.1]`, `0.0.0.0`, `[::]` classify correctly.
- `SecretUrl` and `RedactedUrl` (`scheme://host[:port]/path`) implement Debug/Display redacted and no Serialize. Path-embedded tokens survive redaction (documented residual, THREAT_MODEL SEC-01).

---

## 5. Disk I/O and durability

### 5.1 Staging file
- Name `<target_dir>\<file_name>.<id8>.forgedm.part`, same volume as the final file (atomic rename).
- Created with read+write+`create_new` and share mode `FILE_SHARE_READ`. `create_new` adds `FILE_FLAG_OPEN_REPARSE_POINT`, so a planted link fails with AlreadyExists and a new suffix is chosen. Never `append(true)` (Windows ignores the write offset); never `FILE_FLAG_OVERLAPPED` (std aborts on pending I/O).
- After creation: `ensure_within` (GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED) must be under the canonical `target_dir`, else delete our just-created file (by handle, never by name) and fail the download with `failed(path_escape)` plus a `forgedm::security` path-escape event (PLAN section 11.1 PCR-02, decided). Store `part_file_id` (FileIdInfo).
- Reopen on resume without create, with `FILE_FLAG_OPEN_REPARSE_POINT`: not a reparse point, id matches, length >= max(committed); else `part_file_invalid` -> restart.

### 5.2 Preallocation
- `volume_info` decides. NTFS segmented mode: `FSCTL_SET_SPARSE` (non-admin; verified 5 GiB logical used 64 KiB) to avoid the valid-data-length zero-fill stall [oldnewthing-vdl]; no `set_len`; clearing the flag at finalize is best effort (perf UNVERIFIED). If setting sparse fails: log and continue.
- exFAT/FAT32: accept zero-fill on the writer thread. FAT32 with T >= 4 GiB fails early (`file_system_limit`).
- Free space: `GetDiskFreeSpaceExW` before the first write (remaining + 64 MiB) and every 1 GiB; disk-full mid-transfer pauses.
- `SetFileValidData` is never used (admin; exposes stale disk data).

### 5.3 Writer thread
- One `std::thread` per active download owns the `File` and reads the bounded channel (16) via `blocking_recv`.
- `Data{seg, offset, bytes}`: loop `seek_write` until all bytes are written (short writes are normal); no cursor I/O on this handle (`seek_write` moves the cursor [std-fileext]); then advance `written`.
- `Flush`: `sync_data()` (FlushFileBuffers), reply with the `written` frontier snapshot (FIFO queue, so it covers every earlier write).
- `Close`: `sync_all()`. I/O errors are typed and fail the download; the writer never panics; no async thread blocks on disk.

### 5.4 Durability invariants
- **DUR-1** A byte is written only after its response passes section 4.6, and only inside its segment's claimed window.
- **DUR-2** Writes within a segment apply in stream order; `written` advances only after a complete positional write.
- **DUR-3** `committed` is set only from a frontier returned after a successful `sync_data()`: committed <= flushed, always.
- **DUR-4** A checkpoint is one `BEGIN IMMEDIATE` transaction rewriting segments and `bytes_committed`; segments disjoint, covering [0, T), `committed <= end`.
- **DUR-5** Clients see `committed_bytes` only after COMMIT.
- **DUR-6** `committed` never decreases except in an explicit restart transaction (reset segments, `set_len(0)`) that runs before any new write.
- **DUR-7** Recovery trusts only `[start, committed)`; on-disk length is never a resume point.
- **DUR-8** Checkpoints every 2 s (time-based) and on pause, cancel, shutdown, error, completion. Lazily persisted splits are safe by DUR-3/DUR-4.
- **DUR-9** A state change with an external side effect (entering `finalizing`, each rename candidate, delete) is persisted with FULL sync before the effect and confirmed after it.
- **DUR-10** The agent deletes only files it created and recorded: path inside `target_dir`, `part_file_id` match, not a reparse point.

**What the tests prove:** DUR-3 is proven by the operation-order test `commit_never_exceeds_last_flush` plus the fault test `sync_data_failure_blocks_commit`. TerminateProcess kill tests prove DUR-1/DUR-6/DUR-7 and resume correctness against process death only: the OS cache survives a process kill, so a kill cannot detect a missing FlushFileBuffers. Power loss is not simulated; it relies on FlushFileBuffers and SQLite FULL semantics [ms-flush], [sqlite-sync] and is recorded "not tested".

### 5.5 Verify and finalize (ADR-015)
**`verifying`** (blocking thread, verify concurrency 2):
1. All segments committed; writer `Close` (`sync_all`).
2. Length == T, else `size_mismatch`; unknown length -> `length_verified = 0`.
3. SHA-256 always computed by streaming the `.part` from disk in 1 MiB reads (correct across pause/kill/resume and verifies what is on disk). Mismatch with `checksum_expected` -> `failed(checksum_mismatch)`, `.part` kept. Store `checksum_actual`.

**`finalizing`** (state persisted with FULL sync first):
4. Write a sanitised `Zone.Identifier` onto the **`.part`** (section 5.6) and flush; the stream moves with the rename, so the final name is never visible without MOTW. No ADS support -> `motw_status = unsupported_fs`, continue.
5. For each candidate `name.ext`, `name (1).ext` ... `name (999).ext`: persist `finalize_target = candidate` (FULL), then `MoveFileExW(\\?\part, \\?\candidate, MOVEFILE_WRITE_THROUGH)` **without** `MOVEFILE_REPLACE_EXISTING`. 183/80 -> next candidate; 5/32 -> backoff from 100 ms up to 10 s, then `finalize_locked`. `std::fs::rename` is banned (replaces existing files).
6. Persist `final_path`.
7. IAE on the final path via `MotwService` (STA thread, 60 s timeout); state stays `finalizing` ("Scanning"). Outcomes: marked -> `motw_status = marked`; failure with file present -> keep the direct stream (`marked_direct`); file removed after `E_FAIL` -> `failed(blocked_by_antivirus)`; `INET_E_SECURITY_PROBLEM` -> `failed(blocked_by_policy)`; timeout -> worker poisoned, direct stream kept.
8. `completed`.
- Collision policy: `rename` (default) and `fail` (tests). A completed file is never overwritten.
- NTFS keeps the file id across a same-volume rename, which reconcile relies on (S1.5 `part_file_id_survives_same_volume_rename`).

### 5.6 Mark-of-the-Web (ADR-015)
- **Direct write:** `<path>:Zone.Identifier` = `[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=<s>\r\nHostUrl=<s>\r\n` then flush. `<s>` = `scheme://host[:port]/path` without userinfo, query, fragment (D8; IAE and Chrome otherwise persist query tokens, verified). Non-http(s) or overlong -> `about:internet`. CR/LF rejected. ReferrerUrl omitted when none.
- **IAE:** dedicated STA thread (`CoInitializeEx(COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)`; MTA fails with `E_NOINTERFACE`, verified); SetClientGuid (fixed ForgeDM GUID), SetLocalPath, SetSource (sanitised), SetReferrer (if any), Save. A blocked Save cannot be cancelled, so on timeout the worker is marked poisoned and later items use the direct write until it recovers. Zero-byte files skip IAE (IAE on an empty file can delete it). The service never blocks other downloads.

### 5.7 Destination paths
- `target_dir`: absolute; not a device path (`\\.\`, `\\?\GLOBALROOT`); fixed or removable local volume (UNC deferred); exists and is a directory; stored canonicalised in `\\?\` form. Name-surrogate reparse points below it are rejected; cloud placeholders (OneDrive) are allowed.
- Direct Win32 calls use `platform::wide` verbatim paths (std's long-path handling does not apply to them); the sanitiser guarantees verbatim names equal normalised ones.
- Full TOCTOU defence (`NtCreateFile` with RootDirectory) is out of Phase 1 scope; `ensure_within` after create is the mitigation.

---

## 6. State machine and reconcile

### 6.1 States and transitions
Canonical: PHASE_0_1_PLAN A.6 (14 states, transitions, command semantics). Pure implementation: `core::state::transition` and `core::state::on_startup` (S1.1). ADR-020: `interrupted` is persisted and set only by shutdown/reconcile (spec section 5 names it); `finalizing` is a persisted state so rename and MOTW can be reconciled after a crash.

### 6.2 Ownership
Commands for a download are serialized through its single owner (Engine actor for non-admitted, DownloadTask for admitted) and every DB transition is a compare-and-set on the `from` state (storage `transition_cas_conflict_on_stale_from_state`). Replies carry the resulting state.

### 6.3 Startup reconcile (before any request is served)
1. One transaction: `probing`, `downloading`, `verifying` -> `interrupted` (reason `crash`, unless shutdown already set `agent_shutdown`); `pausing` -> `paused`; `retry_waiting` kept (timer from `next_retry_at_ms`); `finalizing` handled in step 2. An event is recorded for each.
2. Per download with a recorded part file:

| Condition | Action |
|---|---|
| `finalizing`, `.part` present at `part_path` with matching `part_file_id` | redo finalizing from the Zone.Identifier write (idempotent) |
| `finalizing`, `.part` absent, file at `final_path` or `finalize_target` whose file id equals `part_file_id` | `completed` (IAE re-run if `motw_status` is not `marked`) |
| `finalizing`, no file with matching id anywhere | `failed(part_file_invalid)` (A.6 edge; A.7 row per PLAN section 11.1 PCR-01, decided). The user's Retry restarts from 0. A same-name/same-size foreign file is never adopted |
| invalid segment rows (overlap, gap, committed > end, decode error, unknown state string) | reset and restart (`metadata_corrupt`) |
| `.part` missing, reparse point, id mismatch, or shorter than max(committed) | reset and restart (`part_file_invalid`) |
| non-resumable `transfer_mode` with progress | reset |

3. `interrupted` -> `queued` when auto-resume is on (default); otherwise it stays visible.
4. Unknown `*.forgedm.part` files are never scanned for or deleted.
5. A resumed download always re-probes and checks identity (section 4.7) before writing; a fully committed one goes straight to `verifying`.

---

## 7. Storage (`forgedm-storage`, ADR-011)

### 7.1 Connection and pragmas
- `SqliteConnectOptions::new().filename(p).create_if_missing(true).journal_mode(Wal).synchronous(Full).foreign_keys(true).busy_timeout(5s).pragma("journal_size_limit", "67108864")`.
- FULL, not NORMAL: WAL+NORMAL can roll back recent commits after power loss, unsafe for side-effect states (DUR-9); cost is about one commit per 2 s per download.
- Writer pool `max_connections(1)`, every write `begin_with("BEGIN IMMEDIATE")` (avoids deferred-to-write `SQLITE_BUSY`). Reader pool 4 with `query_only = ON` (per-connection application UNVERIFIED).
- Runtime `sqlx::query(..).bind(..)` / `query_as` + `FromRow` with `&'static str` SQL only; no `query!` macros (no `DATABASE_URL`); `AssertSqlSafe` banned (`check.ps1 :: no-assert-sql-safe`) [sqlx-cl].
- Startup asserts `sqlite_version() >= 3.37.0` (STRICT tables).

### 7.2 Schema v1 (`migrations/0001_init.sql`)
Enum-valued columns are validated in Rust (A.6/A.7), not by CHECK, because SQLite cannot alter a CHECK without rebuilding the table.
```sql
CREATE TABLE downloads (
  id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36),          -- UUIDv7
  state TEXT NOT NULL, state_reason TEXT,
  source TEXT NOT NULL,                                          -- handshake client_kind
  priority INTEGER NOT NULL DEFAULT 1 CHECK (priority BETWEEN 0 AND 2),
  queue_position INTEGER NOT NULL,                               -- gaps of 1024
  original_url_sealed BLOB NOT NULL, final_url_sealed BLOB,      -- DPAPI, section 7.5
  url_display TEXT NOT NULL,                                     -- redacted
  target_dir TEXT NOT NULL, requested_name TEXT, display_name TEXT NOT NULL, file_name TEXT,
  part_path TEXT, part_file_id BLOB, finalize_target TEXT, final_path TEXT,
  collision_policy TEXT NOT NULL DEFAULT 'rename', transfer_mode TEXT,
  total_size INTEGER CHECK (total_size IS NULL OR total_size >= 0),
  mime TEXT,
  etag TEXT, etag_strong INTEGER CHECK (etag_strong IN (0,1)),
  last_modified TEXT, last_modified_strong INTEGER CHECK (last_modified_strong IN (0,1)),
  checksum_expected TEXT CHECK (checksum_expected IS NULL OR (length(checksum_expected) = 64 AND checksum_expected NOT GLOB '*[^0-9a-f]*')),
  checksum_actual TEXT CHECK (checksum_actual IS NULL OR length(checksum_actual) = 64),
  length_verified INTEGER CHECK (length_verified IN (0,1)),
  max_connections INTEGER NOT NULL DEFAULT 8 CHECK (max_connections BETWEEN 1 AND 16),
  rate_limit_bps INTEGER CHECK (rate_limit_bps IS NULL OR rate_limit_bps >= 1024),
  bytes_committed INTEGER NOT NULL DEFAULT 0 CHECK (bytes_committed >= 0),
  retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count >= 0), next_retry_at_ms INTEGER,
  motw_status TEXT,
  last_error_code TEXT, last_error_message TEXT, last_error_retryable INTEGER CHECK (last_error_retryable IN (0,1)),
  created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL, completed_at_ms INTEGER
) STRICT;
CREATE INDEX downloads_sched ON downloads(state, priority DESC, queue_position);
CREATE INDEX downloads_created ON downloads(created_at_ms DESC, id);

CREATE TABLE segments (
  download_id TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
  seg_no INTEGER NOT NULL CHECK (seg_no >= 0),
  start_offset INTEGER NOT NULL CHECK (start_offset >= 0),
  end_offset INTEGER,                                            -- exclusive; NULL = unknown-length single stream
  committed_offset INTEGER NOT NULL,
  retry_count INTEGER NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (download_id, seg_no),
  CHECK (end_offset IS NULL OR end_offset >= start_offset),
  CHECK (committed_offset >= start_offset),
  CHECK (end_offset IS NULL OR committed_offset <= end_offset)
) STRICT, WITHOUT ROWID;

CREATE TABLE download_events (
  id INTEGER PRIMARY KEY,
  download_id TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
  at_ms INTEGER NOT NULL, kind TEXT NOT NULL, from_state TEXT, to_state TEXT, code TEXT,
  detail_json TEXT CHECK (detail_json IS NULL OR (json_valid(detail_json) AND length(detail_json) <= 4096))
) STRICT;
CREATE INDEX download_events_by_download ON download_events(download_id, id);

CREATE TABLE settings (
  key TEXT PRIMARY KEY NOT NULL,                                 -- closed Rust enum
  value_json TEXT NOT NULL CHECK (json_valid(value_json) AND length(value_json) <= 16384),
  updated_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
```
- `download_events`: 200 rows per download, pruned inside the checkpoint transaction.
- Spec tables `queues`, `queue_items`, `schedules`, `host_policies`, `categories`, `external_tools` and columns such as `kind`, `category`, request options arrive in later additive migrations (`ALTER TABLE ... ADD COLUMN`).
- Segment tiling (disjoint, covering [0, T)) is validated in Rust at load.

### 7.3 Repository
`insert`, `get`, `list(states, limit, offset)`; `transition(id, from, to, reason, event)` = `UPDATE ... WHERE id = ? AND state = ?` + event insert in one transaction, rows affected != 1 -> `Conflict`; `save_probe`; `save_checkpoint` (delete + reinsert segments and `bytes_committed` in one IMMEDIATE transaction); `reset_segments`; `set_finalize_target`; `complete`; `delete`; settings; reconcile queries. `agent::store_adapter` maps these onto core's `DownloadStore`.

### 7.4 Migrations
`sqlx::migrate!("./migrations")`; only the agent migrates (SQLite migration locks are no-ops); forward-only; `NNNN_name.sql`, each in its own transaction; applied files never edited; `*.sql text eol=lf` prevents CRLF checksum mismatches; `build.rs` `rerun-if-changed=migrations`. A DB newer than the binary -> exit 5. Pre-migration backups are deferred until a second migration exists.

### 7.5 Secrets at rest (ADR-012)
- `original_url_sealed`, `final_url_sealed` (and future request options) are DPAPI CurrentUser blobs: format `0x01 || blob`, `CRYPTPROTECT_UI_FORBIDDEN`, no prompt struct (removed April 2027 [ms-dpapi]), entropy `"ForgeDM/v1/<column>/<download_id>"` binding each value to its row and column. Output freed with `LocalFree`; plaintext zeroed. `CRYPTPROTECT_LOCAL_MACHINE` is never used.
- `forgedm-storage` defines `SecretSealer`; `agent::sealer` implements it over `platform::dpapi`; storage tests use a test sealer that also enforces context binding.
- Protects against other users and offline disk theft; not against same-user malware. Unseal failure (e.g. after an admin password reset) -> `secrets_unavailable`; the user re-adds the URL.
- Plaintext: `url_display`, paths, names. Listing never unseals. No plaintext lookup is needed in Phase 1.

### 7.6 Redaction
Logs, events, errors and `download_events` carry URLs only as `RedactedUrl`. Never logged or emitted: header values, cookies, credentials, bodies, DPAPI blobs, raw Content-Disposition (only the sanitised name). `Sensitive<T>` prints `[redacted]`. reqwest errors are stripped with `Error::without_url()`. Canary test: PHASE_0_1_PLAN S1.14 `secret_canary_absent`.

---

## 8. Errors and logging

| Crate | Error type |
|---|---|
| protocol | `DecodeError`; wire enums `ErrorCode`, `DownloadErrorCode` are `#[non_exhaustive]` for clients |
| platform | `PlatformError{Win32{api, code}, InvalidInput, Io, ...}` (all Phase 1 variants defined in S0.4); never panics |
| core | `EngineError{Http, Range, RemoteChanged, Redirect, Io{op, kind, raw_os}, Store, Cancelled(Reason), Platform}` with `disposition()` (single classification point, section 4.10) |
| storage | `StoreError{Sqlx, Migrate, Conflict, Corrupt, Crypto}` |
| ipc | `IpcError{NotRunning, Busy, Timeout, PeerRejected, Protocol, FrameTooLarge, Closed, Io, StartFailed{exit_code}}` |
| agent | `AgentError` + `to_wire()` (the only mapping to wire codes; internals logged with a correlation id) |
| desktop | `CommandError` serialised `#[serde(tag = "kind", content = "message")]` |

anyhow only in bin `main.rs`. Logging: tracing-subscriber (env-filter, JSON) -> tracing-appender daily file via the non-blocking writer (guard held by `main`), `max_log_files(14)`; default `info`; redaction at every level; standard fields `download_id`, `seg`, `range`, `status`, `code`, `host` (origin only), `attempt`, `elapsed_ms`; security events on target `forgedm::security`. The CLI logs to stderr only with `-v`; logging init takes a sink so the Phase 3 native host never writes to stdout.

---

## 9. Testing architecture

| Tier | Where | Covers |
|---|---|---|
| T0 unit | `#[cfg(test)]`, crate `tests/` | parsers, validators, decision tables, transition table, sanitiser, redaction, redirect policy, token bucket (paused time), backoff (seeded), EWMA, codec |
| T1 property | proptest | range parsing round-trip/never panics; segment-table model (random claim/split/release/flush/commit preserve DUR-1..DUR-7 and tiling); sanitiser properties; random frames; random command sequences |
| T2 component | `crates/*/tests` | engine + fixture + temp SQLite; migrations and CAS; IPC over real pipes |
| T3 process | `agent/tests`, `cli/tests` via `CARGO_BIN_EXE_*` | kill/restart, CLI flows, bootstrap; agent located as sibling; fail (not skip) if missing |
| T4 slow | `#[ignore]` | > 4 GiB, IAE, soak; run at gates with `--include-ignored` |
| T5 scripted/manual | `scripts/windows-smoke.ps1`, `scripts/phase1-proof.ps1` | real binaries, SHA-256 comparison + request-log assertions in TEST_EVIDENCE |

**Fixture (`tests/fixtures`):** axum 0.8 server per test on `127.0.0.1:0`, route `/f/{seed}/{size}?flags&t=<test-id>` (flag list: PHASE_0_1_PLAN S0.5 + S1.4); raw TCP tier for wire violations hyper will not emit; request log (method, Range, If-Range, Authorization/Cookie presence, connection id, peer port, timestamp, concurrency high-water mark). Content is a splitmix64 counter generator: O(1) at any offset, never repeating (exposes swapped segments), > 4 GiB without storage; verification streams against the generator and reports the first mismatching offset. Cross-host cases use `.test` names via `resolve()`; never `localhost` (may try `::1` first).

**Phase 1 required tests** (names in PHASE_0_1_PLAN): 1/4/8-worker identity; kill/restart with pre-restart DB-vs-generator check and request-log resume bounds; mutated ETag and sampling; ranges ignored -> sequential; 429/503 Retry-After, cooldown, budgets, cancel during backoff; two-client pause/resume/cancel race; > 4 GiB offsets, Content-Range above u32::MAX, ignored 5 GiB run; filename vectors, collisions, injected disk-full, migration, reconcile, corrupt rows; bounded memory (`writer_backpressure_bounded_queue`, working-set measurement with thresholds set from a measured baseline); security tests per THREAT_MODEL.

**Tooling (approved at GATE 1, D4):** cargo-llvm-cov + llvm-tools-preview (>= 80% for protocol, core, storage; else "not measured"); cargo-deny (`deny.toml`; cargo-audit is not used, not installed); cargo-fuzz needs nightly, so proptest substitutes.

---

## 10. Phase 2+ interfaces kept open
Full list: PHASE_0_1_PLAN section 9. Architecture-specific points:
- Tauri progress moves to `tauri::ipc::Channel`; every new command is added to the AppManifest list (`manifest_matches_registered_commands`).
- Phase 3 native host: reuses `forgedm-ipc`, `native_host` client kind with image binding and restricted actions, DNS-resolved destination policy with a pinned resolver (`ClientBuilder::dns_resolver`; `resolve()` overrides bypass the resolver, so tests need an injectable resolver), approved-roots list, idempotency keys.
- Media: HTTP code stays under `core::http` and `core::transfer`; a later `downloads.kind` column.
- Queues/scheduler via `queue_position` gaps and additive tables; settings UI via the closed settings enum.
- Credentials/cookies/custom headers via sealed request options and origin-bound header scoping (section 4.9).
- HTTP/2 and per-host bandwidth caps via a client factory keyed by policy and the limiter chain.
- Installer: `forgedm-agent.exe` next to the desktop exe (identity check depends on it); `crt-static` decision for agent and CLI.

---

## 11. ADRs

| ADR | Decision |
|---|---|
| 001 | Repo root is the Cargo workspace root; resolver 3, edition 2024, MSRV 1.94, toolchain 1.100.0; desktop is a member (so `icon.ico` must exist for workspace builds). |
| 002 | New `forgedm-ipc` crate: one audited transport for agent, CLI, desktop, native host. |
| 003 | Wire envelopes/DTOs are separate from domain structs; shared value enums (state, error codes, priority) defined once in `protocol`; `core` and `storage` are independent; agent adapts storage to core's port. |
| 004 | JSON with u32-LE length prefix and 1 MiB cap (not MessagePack); pure codec in protocol, async transport in ipc. |
| 005 | Pipe security: explicit SDDL (NU deny, user only, ME label), `first_pipe_instance`, remote rejection per instance, 17-instance capacity invariant with replacement-before-handoff, bidirectional SID checks, client full-path server check, no token. Residual: cross-user squat is a DoS. |
| 006 | Lock file for single instance (not a mutex), with 3 s startup retry. |
| 007 | reqwest 0.13.5 rustls/aws-lc-rs with prebuilt NASM (D1; fallback native-tls); HTTP/1.1 only; no decompression; no total timeout; no Referer; identity encoding. |
| 008 | Manual redirects through a pure per-hop policy (downgrade, loop, limit, private destination, header scoping). |
| 009 | Probe = HEAD hint + mandatory `bytes=0-0` GET; probe stream reuse deferred. |
| 010 | One segment-table mutex per download; writer thread per download; sparse `.part` on NTFS. |
| 011 | sqlx runtime queries with static SQL; WAL + FULL; one writer connection with BEGIN IMMEDIATE; STRICT tables; enums validated in Rust; agent-only forward migrations; unix ms. |
| 012 | Per-value DPAPI with row/column entropy; AES-GCM envelope only if per-value cost proves significant. |
| 013 | Custom tokio token bucket, not governor. |
| 014 | In-house retry with injectable clock and RNG; retries restart from the claim. |
| 015 | Verify by streaming SHA-256 from disk; Zone.Identifier on `.part` before rename; no-replace `MoveFileExW` with each candidate persisted; IAE on an STA thread after rename with timeout and poison fallback. |
| 016 | `windows = "=0.62.2"`, used only from `forgedm-platform`; windows-sys transitive only. |
| 017 | Upper-case doc names per the execution prompt. |
| 018 | Isolated test agents via `--data-dir` with a derived pipe name; no test-only security bypass. |
| 019 | StateHub actor with ring buffer; `Subscribe(after_seq)` replays in the registering actor turn. |
| 020 | 14 states incl. persisted `interrupted` and `finalizing`; startup reconcile is file-id based. |
| 021 | Resume identity: equal total + strong validator with If-Range; weak/none -> 64 KiB sample verification; change -> `remote_changed`. |
| 022 | Desktop: ACL-gated app commands via AppManifest, no plugins, strict CSP verified on debug builds, `freezePrototype`, no global Tauri. |
| 023 | Redaction by type (`SecretUrl`, `RedactedUrl`, `Sensitive<T>`), `without_url()` on reqwest errors, canary test. |

Deviations from the spec are listed once in PHASE_0_1_PLAN section 2.

---

## 12. UNVERIFIED register

**Open (to be settled by the named slice or recorded NOT RUN):**
- `AWS_LC_SYS_PREBUILT_NASM` build success; build scripts with a space in the repo path (S0.1).
- ~~Whether pinning channel 1.100.0 triggers a toolchain download (S0.1).~~ Resolved 2026-11-15: yes. The pin names a separate rustup toolchain, even though installed stable is also 1.100.0. The coordinator installs it once (approved at GATE 1).
- Tauri `generate_context!`/`generate_handler!` under `unsafe_code = "deny"` (S0.10).
- `pnpm tauri icon` with SVG input; ESLint flat-config keys for react-hooks 7 / react-refresh 0.5; pnpm 10 settings in `.npmrc` vs `pnpm-workspace.yaml` (S0.10).
- IPC fallback behaviour if `connect-src` lacks `ipc:` (S0.10).
- Bundled SQLite version in libsqlite3-sys 0.37.0 (runtime check >= 3.37, S0.6); `query_only` applied per pooled connection (S0.6).
- Low-integrity client test via `icacls /setintegritylevel low` without admin (S0.7); symlink creation privilege with Developer Mode (S1.5).
- Whether hyper exposes `Transfer-Encoding` to distinguish chunked from close-delimited bodies (S1.7).
- Whether `127.0.0.2` loopback works on Windows (fixture).
- Clearing the sparse flag and NTFS VDL behaviour with sparse files (S1.11 benchmark).
- Per-value DPAPI performance (S1.15 measurement).
- IAE with `\\?\` paths; opening `path:Zone.Identifier` via std at >= 248 chars; SmartScreen use of HostUrl; SaveZoneInformation policy honoured only via IAE (S1.6).
- Whether `SHGetKnownFolderPath` needs COM initialisation (S0.4).
- Session-end notification for a windowless agent (Phase 2); agent survival when a terminal's job object closes (S1.8).
- HTTP/1.1 multi-connection vs HTTP/2 throughput (Phase 5 benchmark).
- NSIS toolchain download on first bundle (Phase 2; avoided by `--no-bundle`).

**Resolved 2026-11-15 (docs.rs / crates.io):** futures-util 0.3.34 exists; reqwest 0.13.5 has `Error::without_url`, `resolve_to_addrs`, `dns_resolver`, `http1_max_headers`, `tls_backend_rustls`, and both `danger_*` (deprecated) and `tls_danger_*` names; tracing-appender 0.2.5 has `rolling::Builder::max_log_files`; rcgen latest 0.14.10; tokio-rustls 0.26.6 is an optional reqwest dependency under `rustls`; ts-rs 12.0.1 defaults large ints to `bigint`.

---

## 13. References
- **Research reports** [r-crates], [r-win32], [r-tauri], [r-prior] (2026-11-15), incl. non-admin probes on Windows 11 build 26300: sparse files, pipe DACL readback, FIRST_PIPE_INSTANCE, IAE STA vs MTA, MoveFileExW error codes, reserved names, pipe namespace.
- **[crates-api]** https://crates.io/api/v1/crates/{name}?include=default_version and `/versions?sort=semver`
- **[npm]** https://registry.npmjs.org/ (package metadata queried 2026-11-15)
- **[rs-channel]** https://static.rust-lang.org/dist/channel-rust-stable.toml
- **[node-sched]** https://raw.githubusercontent.com/nodejs/Release/main/schedule.json
- **reqwest:** [rq-features] https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html ; [rq-src] https://static.crates.io/crates/reqwest/reqwest-0.13.5.crate ; [rq-redirect] https://github.com/seanmonstar/reqwest/blob/v0.13.5/src/redirect.rs ; [rq-changelog] https://raw.githubusercontent.com/seanmonstar/reqwest/master/CHANGELOG.md ; [rq-proxy] https://static.crates.io/crates/hyper-util/hyper-util-0.1.21.crate (`src/client/proxy/matcher.rs`)
- **[aws-lc-win]** https://aws.github.io/aws-lc-rs/requirements/windows.html
- **sqlx:** [sqlx-cl] https://raw.githubusercontent.com/launchbadge/sqlx/main/CHANGELOG.md ; [sqlx-src] https://static.crates.io/crates/sqlx-sqlite/sqlx-sqlite-0.9.0.crate ; https://docs.rs/sqlx/0.9.0/sqlx/sqlite/struct.SqliteConnectOptions.html ; https://docs.rs/sqlx/0.9.0/sqlx/macro.migrate.html
- **[tokio-pipe]** https://docs.rs/tokio/1.54.1/tokio/net/windows/named_pipe/struct.ServerOptions.html ; https://docs.rs/tokio/1.54.1/tokio/net/windows/named_pipe/struct.ClientOptions.html
- **windows-rs:** [win-src] https://raw.githubusercontent.com/microsoft/windows-rs/71/crates/libs/windows/Cargo.toml ; [win-master] https://raw.githubusercontent.com/microsoft/windows-rs/master/crates/libs/windows/Cargo.toml
- **[hybrid-array]** https://static.crates.io/crates/hybrid-array/hybrid-array-0.4.15.crate
- **Microsoft Learn:** named-pipe security https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights ; https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getnamedpipeserverprocessid ; [ms-mutex] https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createmutexw ; https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderpath ; [ms-dpapi] https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata ; https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-iattachmentexecute ; https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw ; [ms-flush] https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers ; https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_set_sparse ; https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
- **Chromium MOTW:** https://raw.githubusercontent.com/chromium/chromium/main/components/services/quarantine/quarantine_win.cc
- **Rust std:** [std-fileext] https://doc.rust-lang.org/std/os/windows/fs/trait.FileExt.html ; https://doc.rust-lang.org/std/fs/struct.File.html ; https://raw.githubusercontent.com/rust-lang/rust/1.100.0/library/std/src/sys/fs/windows.rs
- **[oldnewthing-vdl]** https://devblogs.microsoft.com/oldnewthing/20110922-00/?p=9573
- **RFCs:** https://www.rfc-editor.org/rfc/rfc9110.txt (sections 8.8, 10.2.3, 13.1.5, 14, 15.3.7, 15.5.17), https://www.rfc-editor.org/rfc/rfc9112.txt (section 8), https://www.rfc-editor.org/rfc/rfc6266.txt
- **[sqlite-sync]** https://www.sqlite.org/pragma.html#pragma_synchronous
- **[aws-backoff]** https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/
- **[indicatif]** https://github.com/console-rs/indicatif/blob/main/src/state.rs
- **Tauri:** [tauri-acl] https://github.com/tauri-apps/tauri/blob/tauri-v2.12.5/crates/tauri/src/webview/mod.rs ; [tauri-csp] https://github.com/tauri-apps/tauri/blob/tauri-v2.12.5/crates/tauri/src/manager/webview.rs ; https://github.com/tauri-apps/tauri/blob/tauri-build-v2.7.4/crates/tauri-build/src/lib.rs ; https://v2.tauri.app/security/capabilities/ ; https://v2.tauri.app/security/csp/ ; https://v2.tauri.app/distribute/windows-installer/ ; [tauri-crates] https://crates.io/api/v1/crates/tauri/2.12.5/dependencies
- **Repo docs:** `ForgeDM_Claude_Master_Spec_v1.1_UI_Enhanced.md`, `ForgeDM_Claude_Code_ECC_Execution_Prompt.md`, `docs/ECC_WORKFLOW.md`
