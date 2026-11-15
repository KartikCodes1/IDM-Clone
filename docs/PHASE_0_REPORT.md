# ForgeDM Phase 0 Boundary Report

**Date:** 2026-11-16
**Scope:** Phase 0 at commit 18f48c0 (main branch), evaluated in the detached worktree C:\Users\karti\Desktop\forgedm-p0gate
**Gate verdict:** PASSED WITH OPEN ITEMS

Phase 0 is not marked complete on the strength of this report alone. Confirmed findings are NOT fixed yet; the coordinator schedules the fixes. The verdict assumes no confirmed CRITICAL or HIGH finding, and it depends on one open security item whose severity is not assigned in the input data (see section 7).

---

## 1. Changed files and areas

Range: `git log 55a01c5..18f48c0` (26 commits, from a47b9d5 to 18f48c0). Total from `git diff --shortstat 55a01c5..18f48c0`: 290 files changed, 44890 insertions.

| Area | Files changed |
|---|---|
| apps/desktop (Tauri shell, frontend, tests) | 53 |
| crates/forgedm-protocol | 77 |
| crates/forgedm-core | 30 |
| crates/forgedm-agent | 22 |
| crates/forgedm-ipc | 15 |
| crates/forgedm-platform | 17 |
| crates/forgedm-storage | 19 |
| crates/forgedm-cli | 12 |
| tests/fixtures | 23 |
| docs (ARCHITECTURE, THREAT_MODEL, PLAN, SECURITY_REVIEW, TEST_EVIDENCE, IMPLEMENTATION_STATUS, NEXT_STEPS, DEVELOPMENT, ECC_WORKFLOW) | 9 |
| Root (Cargo.toml, Cargo.lock, package.json, pnpm-lock.yaml, pnpm-workspace.yaml, rust-toolchain.toml, clippy.toml, .cargo/config.toml, .gitattributes, .gitignore, CLAUDE.md, spec and execution prompt) | 14 |

Phase 0 commits by slice: S0.1 scaffold (a47b9d5), S0.2 docs (b1e8727), S0.3 protocol (02c4656), S0.4 platform (72520f6, e576328, 93c8bc9), S0.5 fixture (23472a8), S0.6 storage (344d8a7), S0.7 IPC (b4d96e9), S0.8 agent (0ad7d09), S0.9 CLI (4045f2e, 7d7f032), S0.10 desktop (118bd1b, cad5ec7), S0.11 gate evidence (18f48c0). Wave work was also committed for core, the fixture, the platform MotW and safe file primitives (4b203ac, 8626f6b, 8389c91, a816d02) and for Phase 1 re-sequencing (aaeb099).

**Not committed:** the gate scripts scripts\check-env.ps1, scripts\check.ps1 and scripts\windows-smoke.ps1 are untracked. Byte-identical copies (same sha256) exist in the main checkout and in the worktree at scripts\. They are not in commit 18f48c0.

---

## 2. Working behaviour and how to run it

What works and was run in this gate:
- Typed protocol v1 (frames, envelopes, golden files), named-pipe IPC with DACL and mutual identity checks, a fixture server, SQLite storage with DPAPI-sealed URL columns, an agent with a single-instance lock, a CLI (`status`, `list`, `list --json`, `agent stop`), and a Tauri desktop shell.
- An empty but connected agent: the agent starts, serves CLI requests, refuses a second instance (exit 3), and stops cleanly on `agent stop` (exit 0).
- No download transfer path exists yet. Phase 0 has no real download engine; that begins at S1.7.

Commands (run from the repository root, Git Bash with PATH set as in the gate notes):
```
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --workspace --bins --locked
cargo test --workspace --locked
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows-smoke.ps1
```
Agent and CLI usage observed in the smoke run: `forgedm status` (exit 0, names the agent pid), `forgedm list` (exit 0, prints "No downloads."), `forgedm list --json` (prints {"items":[],"total":0}), `forgedm agent stop` (exit 0). Without a running agent, `status`, `list` and `agent stop` exit 10. An unknown subcommand exits 2. The agent accepts `--data-dir`. The exact agent start invocation is in scripts\windows-smoke.ps1, which this report does not reproduce.

The desktop shell was built with `pnpm --filter @forgedm/desktop tauri build --debug --no-bundle`. A manual run record was not produced (see section 6).

---

## 3. Exact commands and exit codes

Each row is a command actually run in this gate. Paths are as run.

| # | Command | Exit | Notes |
|---|---|---|---|
| 1 | `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-env.ps1` (Git Bash, unquoted) | 127 | Mistake: Bash collapsed the path. The script never ran. |
| 2 | `powershell -NoProfile -ExecutionPolicy Bypass -File 'scripts\check-env.ps1'` (worktree) | 0 | check-env PASS. |
| 3 | `cargo fmt --all --check` | 0 | No differences. |
| 4 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 | 0 warnings or errors. |
| 5 | `cargo build --workspace --bins --locked` | 0 | 0 warnings. Note: PLAN 8.1 shows the form without `--bins`; `--bins` was run. |
| 6 | `cargo test --workspace --locked` | 0 | 448 passed, 0 failed, 1 ignored. |
| 7 | `pnpm install --frozen-lockfile` (Node 26 shim) | 0 | 240 packages reused, 0 downloaded. |
| 8 | `pnpm -r lint` | 0 | Clean. |
| 9 | `pnpm -r typecheck` | 0 | Clean. |
| 10 | `pnpm -r test` | 0 | 3 files, 14 tests passed. |
| 11 | `pnpm audit --prod` | 0 | No known vulnerabilities. |
| 12 | `pnpm --filter @forgedm/desktop tauri build --debug --no-bundle` | 0 | JS 227.26 kB; exe 13,882,880 bytes. |
| 13 | `powershell -NoProfile -ExecutionPolicy Bypass -File 'scripts\check.ps1'` (first version) | 1 | Script bug in eslint-bans (PS 5.1 array nesting). Fixed. |
| 14 | `check.ps1` (final version, worktree) | 0 | 13 PASS, 0 FAIL, 0 NOT RUN. |
| 15 | `check.ps1` (final, rg engine via VS Code's bundled rg 13.0.0 on PATH for this process) | 0 | 13 PASS. |
| 16 | `check.ps1` negative test on planted-violation scratch tree | 1 | Expected. Planted violations caught. A second script bug (Format-Hits) was found and fixed. Re-run: 1 PASS, 9 FAIL, 3 NOT RUN, exit 1. |
| 17 | `check.ps1` negative test, PATH limited to System32 and PowerShell | 1 | Directory-walk fallback works. 1 PASS, 5 FAIL, 7 NOT RUN. |
| 18 | `check.ps1` negative test with rg engine | 1 | All 4 planted violations reported. |
| 19 | `powershell -NoProfile -ExecutionPolicy Bypass -File 'scripts\windows-smoke.ps1'` (first version) | 0 | 22 PASS. Its workspace build replaced the tauri-built desktop exe; scope was narrowed. |
| 20 | `windows-smoke.ps1` (final version) | 0 | 22 PASS, 0 FAIL, 0 NOT RUN. |
| 21 | `cargo llvm-cov --locked -p forgedm-protocol -p forgedm-storage -p forgedm-ipc -p forgedm-platform --summary-only` | 101 | No report. 6 ipc bootstrap tests need forgedm-agent.exe, which was not built in the coverage target. |
| 22 | `cargo llvm-cov ... --summary-only --no-fail-fast --ignore-run-fail` | 1 | Not allowed by cargo-llvm-cov 0.9.1 (flags conflict). Nothing ran. |
| 23 | `cargo llvm-cov ... --summary-only --ignore-run-fail` | 0 | Report produced. The same 6 bootstrap tests failed. Total lines 87.84%. |
| 24 | `cargo deny --locked check advisories` (no deny.toml; default config) | 1 | 2 errors: RUSTSEC-2026-0459 and RUSTSEC-2024-0370. |
| 25 | `cargo tree --locked -i proc-macro-error --target x86_64-pc-windows-msvc` | 0 | Empty: not in the Windows graph. |
| 26 | Memory monitor (scratch helper, 1 GB working-set limit, 500 ms sampling) | 0 | 1003 samples, 81 processes tracked, 0 over 1 GB, 0 killed. Highest 549.3 MB (Node 26 node.exe). |

Also run: `cargo metadata --locked --no-deps` (layering check), `cargo tree --locked` variants (no-fixture-in-shipped, no-fault-injection-in-release, no-reqwest-compression), and the static text searches in check.ps1. Their results are in the check.ps1 table below; the individual exit codes were not recorded separately.

Not run: the Tauri debug re-run that the gate notes flagged as needed after the smoke build. The later note says the desktop exe hash (821a19e5...) was unchanged after the final smoke run, which is the evidence used here.

---

## 4. Test results and counts

**Rust** (`cargo test --workspace --locked`, exit 0): 448 passed, 0 failed, 1 ignored, across 64 result blocks. 0 warning lines.

| Crate | Passed | Ignored |
|---|---|---|
| forgedm-agent | 65 | 0 |
| forgedm-cli | 17 | 0 |
| forgedm-core | 127 | 0 |
| forgedm-desktop | 17 | 0 |
| forgedm-fixture | 63 | 0 |
| forgedm-ipc | 43 | 0 |
| forgedm-platform | 70 | 1 (motw iae_from_sta_writes_zone_identifier, #[ignore] by design) |
| forgedm-protocol | 21 | 0 |
| forgedm-storage | 25 | 0 |
| **Total** | **448** | **1** |

**Frontend:** vitest 3 files, 14 tests passed (`pnpm -r test`).

**Smoke:** windows-smoke.ps1 22 PASS, 0 FAIL, 0 NOT RUN. Peak agent working set 23.3 MB.

**Static checks** (check.ps1, 13 PASS, 0 FAIL, 0 NOT RUN):

| Check | Result |
|---|---|
| no-tls-bypass | PASS (0 hits; negative test caught planted line) |
| no-assert-sql-safe | PASS (0 hits; negative test caught planted line) |
| unsafe-only-in-platform | PASS (0 outside platform; 65 inside as sanity check) |
| command-new-allowlist | PASS (1 in ipc bootstrap.rs, 16 in tests/, 0 elsewhere) |
| layering | PASS (9 packages, 17 internal edges, all allowed) |
| no-fixture-in-shipped | PASS (319 crates in shipped roots; no forgedm-fixture, no axum) |
| no-fault-injection-in-release | PASS (feature declared in agent manifest; not enabled) |
| no-reqwest-compression | PASS (no gzip, brotli, deflate or zstd features) |
| tauri-versions-match | PASS (2.12.x in Cargo.lock and both npm manifests) |
| no-loose-versions | PASS (110 Cargo and 24 npm requirements checked) |
| lockfiles-committed | PASS (unchanged from HEAD) |
| manifest-asinvoker | PASS (no requireAdministrator or highestAvailable; no explicit asInvoker either, see section 7) |
| eslint-bans | PASS (ESLint stdin probe flagged all 4 banned constructs) |

**Windows smoke checks** (windows-smoke.ps1): agent-has-no-tcp-listener PASS (Get-NetTCPConnection empty in 3 samples; a loopback self-test was seen by the same probe; 0 UDP endpoints). Lock held; a second agent exited 3; agent stop exit 0; lock released; restart ready in 28 ms.

**Coverage** (`cargo llvm-cov ... --ignore-run-fail`, informational, exit 0):

| Crate | Lines | Region |
|---|---|---|
| forgedm-protocol | 92.82% (633/682) | 89.20% |
| forgedm-storage | 91.60% (883/964) | 82.54% |
| forgedm-platform | 89.05% (927/1041) | 81.95% |
| forgedm-ipc | 77.24% (604/782) | 72.91% |
| Total (protocol, storage, ipc, platform) | 87.84% (3047/3469) | |

The ipc figure is conservative: bootstrap.rs is at 39.29% because its 6 tests could not run under coverage. Excluding bootstrap.rs, ipc is 83.0%. forgedm-core was not in this run. The 80% line gate formally applies to core, protocol and storage at Phase 1, so this is informational only for Phase 0.

---

## 5. Reviewers and their findings

Reviews were run on worktree 18f48c0, read-only. The security reviewer ran no cargo commands; the other two reviewers ran no commands at all. Their findings were then verified (confirmed, rejected or left unverified) as recorded below.

| Reviewer | Reported | Confirmed | Rejected by verifier | Unverified |
|---|---|---|---|---|
| ecc:security-reviewer | 4 | 2 (LOW) | 0 | 2 (LOW) |
| ecc:pr-test-analyzer | 10 | 4 (1 MEDIUM, 3 LOW) | 1 (MEDIUM) | 5 (LOW) |
| ecc:silent-failure-hunter | 5 | 1 (LOW) | 1 (MEDIUM) | 3 (LOW) |

The security reviewer's summary arrived truncated mid-sentence. The findings above are the ones that survived the record; nothing beyond them is asserted. The reviewer found no Critical or High issues. Its pipe SDDL, caller SID check, client server-identity check, and DPAPI sealing traces were reported as sound. Those traces were not re-run by this report.

pr-test-analyzer rated the missing S0.11 scripts as High. The confirmed record lists it as MEDIUM. The coordinator should confirm this downgrade, because the gate verdict uses the confirmed record.

**Confirmed findings (NOT fixed; coordinator schedules fixes):**

| ID | Sev | Location | Summary |
|---|---|---|---|
| P0-security-reviewer-1 | LOW | crates/forgedm-agent/src/data_dir.rs:63 | An explicit --data-dir is trusted with no owner, DACL or volume check. UNC paths are accepted. The user-only assumption holds only for the default %LOCALAPPDATA%\ForgeDM. Matches the existing open item S0.8-security-reviewer-6. |
| P0-security-reviewer-2 | LOW | docs/THREAT_MODEL.md:344 | Inv-07, 08, 18, 22 and 24 name check.ps1 checks that were not in HEAD (scripts/ and deny.toml absent at 18f48c0). The scripts now exist uncommitted, so this is open until committed. |
| P0-pr-test-analyzer-5 | MEDIUM | scripts/ (absent at HEAD) | S0.11 scripts were missing at HEAD. They were written in this gate and run, but are not committed and have not been re-reviewed. Stays open until committed. |
| P0-pr-test-analyzer-6 | LOW | crates/forgedm-ipc/src/server.rs:331 | Server-side caller SID rejection (IPC-03) has no behavioural test. expected_sid is hard-wired in Server::bind, so a test cannot inject a different SID. |
| P0-pr-test-analyzer-7 | LOW | crates/forgedm-agent/src/app.rs:287 | Exit 4 on a post-startup fatal pipe error is not tested at app level. |
| P0-pr-test-analyzer-9 | LOW | crates/forgedm-agent/tests/process.rs:352 | data_dir_with_reparse_point_exits_6 prints SKIPPED and passes when symlink creation fails. The reparse-point rejection is a security control, so a skipped run is reported as a pass. Line is 352, not 345 as the reviewer said. |
| P0-silent-failure-hunter-16 | LOW | crates/forgedm-cli/src/output.rs:14 | CLI discards all stdout and stderr write errors. `list --json > file` can exit 0 with truncated output. |

**Rejected by verifier (not findings):** P0-pr-test-analyzer-8 (MEDIUM, Inv-07 has no behavioural test) and P0-silent-failure-hunter-15 (MEDIUM, storage_to_wire mapping).

**Unverified LOW (not confirmed, not counted in the verdict):** P0-security-reviewer-3 (agent panics on non-Unicode args before logging), P0-security-reviewer-4 (duplicated identity logic in ipc and platform), P0-pr-test-analyzer-10 (no process test that shutdown settles in-flight states), P0-pr-test-analyzer-11 (secret canary covers two columns and the XOR test sealer only), P0-pr-test-analyzer-12 (binds_loopback_only checks self-reported address), P0-pr-test-analyzer-13 (no pinned generator known-answer vector), P0-pr-test-analyzer-14 (500 ms negative timeout in transport test), P0-silent-failure-hunter-17 (list paging accepts an early empty page and reports success), P0-silent-failure-hunter-18 (desktop IpcError::Server drops the wire error code), P0-silent-failure-hunter-19 (check_child leaks the child on try_wait failure; encode failures unlogged).

---

## 6. Definition of Done (PLAN section 7, Phase 0 rows)

Status key: PASS = evidence in this report shows the requirement. PARTIAL = part of the requirement is shown. FAIL = not met. No row is marked "100% secure"; the security rows below are only as strong as the tests and checks listed.

| Requirement | Evidence | Slices | Status |
|---|---|---|---|
| Environment inspection, toolchain detection | check-env PASS (exit 0): Rust 1.100.0, VS Build Tools, SDK, WebView2, node/pnpm checked. The output is recorded here, not yet in TEST_EVIDENCE. | S0.11 | PARTIAL (TEST_EVIDENCE not updated) |
| Architecture + threat model accepted; ECC selection recorded | ARCHITECTURE and THREAT_MODEL exist at HEAD. Sign-off and ECC_WORKFLOW update were not verified in this run. Open items P0-security-reviewer-2 affects the THREAT_MODEL evidence claims. | S0.2, S0.11 | PARTIAL |
| Workspace scaffold | `cargo build --workspace --bins --locked` exit 0, 0 warnings. The literal PLAN form without `--bins` was not run. | S0.1 | PASS |
| Schema | storage crate: 25 tests passed; storage 91.60% lines. Specific migration tests were not extracted in this run. | S0.6 | PARTIAL |
| Typed IPC | protocol 21 tests passed; ipc 43 tests passed; protocol 92.82% lines. Golden files consumed by TS tests were not verified (the 14 vitest tests were not shown to read them). | S0.3, S0.10 | PARTIAL |
| Local HTTP fixture | fixture 63 tests passed (self-tests included). | S0.5 | PASS |
| Empty but connected agent and CLI | agent 65 + cli 17 tests passed; windows-smoke.ps1 22 PASS, 0 FAIL. The smoke script is untracked (see section 1), and the S0.8 IPC-03 and exit-4 tests are open (P0-pr-test-analyzer-6 and -7). | S0.8, S0.9 | PASS (scripts uncommitted) |
| Minimal Tauri shell, original name and icon | Debug build exit 0 (`tauri build --debug --no-bundle`). Desktop tests 17 (Rust) and 14 (vitest) passed. No manual run record and no icon check were produced. | S0.10 | PARTIAL |
| Hard gate: section 8.1 commands green; zero open Critical/High; status rows filled | All 8.1 commands that were run exited 0 (see section 3), but the literal `cargo build` without `--bins` and the `-ExecutionPolicy Bypass` form were not run as written. No confirmed Critical or High. IMPLEMENTATION_STATUS and TEST_EVIDENCE were not updated. RUSTSEC-2026-0459 severity is not assigned in the input data (section 7). | S0.11 | PARTIAL |

**Section 8.1 command status:**

| Command (PLAN 8.1) | Status |
|---|---|
| check-env.ps1 | PASS (exit 0, with -ExecutionPolicy Bypass) |
| cargo fmt --all --check | PASS (exit 0) |
| cargo clippy --workspace --all-targets --locked -- -D warnings | PASS (exit 0) |
| cargo build --workspace --bins --locked | PASS (exit 0) |
| cargo test --workspace --locked | PASS (exit 0, 448 passed) |
| pnpm install --frozen-lockfile | PASS (exit 0) |
| pnpm -r lint / typecheck / test / audit --prod | PASS (all exit 0) |
| pnpm --filter @forgedm/desktop tauri build --debug --no-bundle | PASS (exit 0) |
| check.ps1 | PASS (exit 0, 13 PASS) |
| windows-smoke.ps1 | PASS (exit 0, 22 PASS) |

---

## 7. Security status and open items

No security claim in this report is "100% secure". The statements below are limited to what was run.

**Checks that passed:** the static checks in section 4 (TLS bypass, SQL assertion, unsafe confinement, Command::new allowlist, layering, shipped-binary contents, reqwest compression, manifest elevation text, lockfile and version hygiene, ESLint bans), and the smoke check that the agent holds no TCP listener.

**Open items:**

| Item | Severity | Status |
|---|---|---|
| RUSTSEC-2026-0459 (CVE-2026-119375): libsqlite3-sys 0.37.0 bundles SQLite 3.51.3. Local attack: a crafted sibling -wal file of a different page size can return uninitialized page-cache memory, and the victim must open the files. Path: sqlx-sqlite 0.9.0 to sqlx to forgedm-storage to forgedm-agent. Fix needs libsqlite3-sys 0.39.0 or later; sqlx-sqlite 0.9.0 caps it below 0.38, so `cargo update` cannot fix it. | Not assigned in the input data. The gate treats it as below HIGH only if the coordinator confirms that. | OPEN. Needs a newer sqlx or a documented risk acceptance. ForgeDM's database is in the user's profile, so exploitation needs write access there. Coordinator decision. Root Cargo files were not changed. |
| RUSTSEC-2024-0370: proc-macro-error 1.0.4 unmaintained; Linux-only (glib-macros and gtk). Empty in the Windows target graph. | Not assigned in the input data. | OPEN for the deny.toml work (S1.15). Target-filtered config should scope it out. |
| P0-security-reviewer-1 (data-dir trust) | LOW | Open (matches S0.8-security-reviewer-6). |
| P0-security-reviewer-2 (THREAT_MODEL cites checks not in HEAD) | LOW | Open until scripts are committed. |
| Observation: no built exe embeds requestedExecutionLevel asInvoker. Inv-22 is met only by absence of elevation strings. | LOW | Open; fits Phase 2 installer hardening. |
| Observation: rolling agent log is named by UTC date (agent.log.2026-11-15 on local date 2026-11-16). | LOW | Open. |
| Unverified LOW items (section 5) | LOW | Not confirmed; not fixed. |

**Not verified:** licenses, bans and sources in cargo-deny (not run); fuzzing; any runtime behaviour of the desktop app beyond its build; the Windows DPAPI canary through the real sealer (covered only by the storage test sealer, see P0-pr-test-analyzer-11 unverified).

---

## 8. Limitations and NOT RUN items

- **cargo llvm-cov (exact command):** exit 101, no report. Numbers come from a re-run with `--ignore-run-fail`. This is a test-harness gap for the ipc bootstrap tests, not a product failure. forgedm-core coverage was not measured.
- **cargo deny check:** advisories only. Licenses, bans and sources: NOT RUN. No deny.toml exists. Advisories failed (exit 1).
- **Desktop manual run record:** NOT RUN. Required by the S0.10 row; not produced.
- **Icon and name check on the Tauri shell:** NOT RUN.
- **TEST_EVIDENCE, IMPLEMENTATION_STATUS and NEXT_STEPS:** NOT updated. Not in scope for this run.
- **Scripts without `-ExecutionPolicy Bypass`:** NOT RUN. Whether the default policy blocks them is unknown.
- **Literal PLAN 8.1 forms:** `cargo build --workspace --locked` (without --bins) and `powershell -NoProfile -File scripts\...` without the bypass flag were not run as written.
- **Phase 1 commands (crash_matrix, `-- --include-ignored` slow tier, phase1-proof.ps1, cargo llvm-cov on core, cargo deny full):** NOT RUN; Phase 1 items.
- **rg engine:** the rg branch of check.ps1 was verified only with VS Code's bundled rg 13.0.0 placed first on PATH for one process. rg is not on the system PATH. Nothing was installed.
- **Honest limits from PLAN section 7:** power-loss durability untested; TerminateProcess kills do not prove FlushFileBuffers; real reboot, real removable-drive pull, and real full-volume tests are manual; cross-user pipe squatting needs a second account (manual, not run); PAC/WPAD unsupported.
- **Shared build output:** builds ran in the shared target\ directory, so target\debug holds worktree Phase 0 binaries until the next main-checkout build re-uplifts. The other workflow's agent (pid 19504, target\s17) was not touched.
- **Smoke isolation:** windows-smoke.ps1 copies forgedm.exe and forgedm-agent.exe to a %TEMP% folder before running them. The CLI's sibling-agent check still holds because both copies share the folder.
- **Scope of this report:** it covers commit 18f48c0 and the worktree. The main checkout has uncommitted S1.7 changes under crates\ that this report does not evaluate.

---

## 9. Next slice

Phase 1 is underway. Slice S1.7 (thin vertical slice: single-stream download through agent and CLI) is in progress in the main checkout in a parallel workflow. It has uncommitted changes under crates\forgedm-core, crates\forgedm-agent and crates\forgedm-cli. This report does not cover S1.7 and does not depend on it.

Before the Phase 0 gate is closed, the coordinator should:
1. Decide the RUSTSEC-2026-0459 severity and either fix it (newer sqlx) or record a risk acceptance.
2. Commit the three gate scripts and re-review them, and confirm the pr-test-analyzer-5 severity (High in review, MEDIUM in the confirmed record).
3. Schedule the confirmed LOW fixes and the test gaps listed in section 5.
4. Update TEST_EVIDENCE and IMPLEMENTATION_STATUS with the runs in section 3.
5. Record the desktop manual run and the icon check, or mark them NOT RUN.

---

## 9a. Coordinator decisions (2026-11-16)

1. **RUSTSEC-2026-0459: rated LOW for ForgeDM; accepted and tracked.**
   - Why LOW: exploiting it needs write access to the agent's data directory. For the default root, `%LOCALAPPDATA%\ForgeDM` in the user's own profile, that already means same-user compromise, which THREAT_MODEL section 8 accepts as out of scope.
   - Why not fixed: no sqlx release yet allows libsqlite3-sys >= 0.39.
   - Tracking:
     - Upgrade sqlx as soon as one does.
     - The S1.15 `deny.toml` ignores this ID with a reason and a review date.
     - The data-dir ownership/ACL check (P0-security-reviewer-1, scheduled in S1.14) closes the explicit `--data-dir` exposure.
2. **RUSTSEC-2024-0370: not applicable on Windows.** The crate is absent from the `x86_64-pc-windows-msvc` graph. The S1.15 `deny.toml` scopes targets to Windows.
3. **Gate scripts:** committed together with this report, which resolves P0-pr-test-analyzer-5 (MEDIUM; the severity downgrade from High is confirmed, because the scripts existed and ran green and were only uncommitted). P0-security-reviewer-2 is resolved for check.ps1. The cargo-deny licences/bans/sources check stays NOT RUN until S1.15.
4. **Confirmed LOW fixes are scheduled:**
   - S1.9 (it owns agent and CLI code):
     - `data_dir_with_reparse_point_exits_6` must not pass vacuously
     - CLI output write errors
     - an IPC-03 server-SID test seam
     - an agent exit-4 app-level test
     - the UTC log-date name
   - S1.14: data-dir ACL/owner check.
   - W7 installer slice: embed an `asInvoker` manifest.
   - Coverage harness: build `forgedm-agent` before `cargo llvm-cov` for ipc (S1.15).
5. **Desktop manual run record:** NOT RUN for Phase 0. It is done with the usable-desktop slice in W7.

## 10. Gate verdict

**PASSED WITH OPEN ITEMS.**

Reason: no confirmed CRITICAL or HIGH finding; all section 8.1 commands that were run exited 0; the Phase 0 rows that need only a run are PASS. Several rows remain PARTIAL, and the gate is not fully closed until items 1 to 5 in section 9 are done. Confirmed findings have NOT been fixed.
