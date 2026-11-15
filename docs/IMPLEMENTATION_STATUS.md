# Implementation Status

Updated 2026-11-15. Statuses: **Implemented** (code exists), **Tested** (tests ran and passed; evidence in `TEST_EVIDENCE.md`), **Partial**, **Deferred**, **Blocked**, **Planned** (not started). A slice counts as done only when it is Tested, its reviews are clean, and it has zero open Critical/High findings.

## Phase 0

| Slice | Title | Status | Evidence / references |
|---|---|---|---|
| S0.1 | Workspace scaffold, toolchain pin, dependency lock | **Tested** (uncommitted) | 8 packages, 326 locked crates, `tls_smoke` 1 passed; security/rust/code reviews: 0 Critical/High/Medium, 6 Low (2 fixed, 4 no change). TEST_EVIDENCE "S0.1". Deviation: `[workspace.lints.cargo] unused_dependencies = "allow"` until the Phase 1 gate (deps are pre-declared by design). |
| S0.2 | Architecture, threat model, development docs | **Implemented** (docs; uncommitted) | GATE 1 approved 2026-11-15. Docs accepted, `DEVELOPMENT.md` written. Security-reviewer and architect sign-off review: 0 Critical/High, 8 Medium confirmed by a verifier, all fixed. PCR-01/02 decided. SECURITY_REVIEW "S0.2". |
| S0.3 | Protocol v1 | **Tested** (uncommitted) | 21 passed, 0 failed (protocol); W1 gate green. Reviews: 0 Critical/High/Medium after verification; 12 Low (7 fixed, 1 no change, 4 not fixed). Deviations: optional `DownloadError.details` added (additive); typed `ErrorBody.details`; serde `Unknown` variants on DownloadState, ErrorCode and DownloadErrorCode; serde flatten on DownloadDetail (result side only). Open: Appendix A.4/A.7 need a PCR or update for details, RecentEvent shape and flatten; Phase 1 agent must reject device/UNC target_dir and sanitize file_name; recover_request_id and decode_json_object are last-wins on duplicate keys. TEST_EVIDENCE S0.3; SECURITY_REVIEW S0.3. |
| S0.4 | Platform security primitives | **Tested** (uncommitted) | 23 passed, 0 failed (security 14, folders 3, dpapi 6); W1 gate green. Reviews: 0 Critical/High; 2 Medium (1 fixed, 1 open decision); 8 Low. Open decision: create_pipe_instance calls CreateNamedPipeW directly, not tokio, because tokio is not a declared dependency and adopting the handle needs unsafe (forgedm-ipc forbids it); a tokio dependency would change Cargo.lock. Coordinator must decide before S0.7, then update PLAN S0.4 and ARCHITECTURE 3.3. Also open: no low-integrity or foreign-principal denial test (cross-user denial verified by DACL readback only); PID-based peer check has a residual reuse risk. TEST_EVIDENCE S0.4; SECURITY_REVIEW S0.4. |
| S0.5 | Fixture server baseline | **Tested** (uncommitted) | 22 passed, 0 failed (unit 6, cli 2, content 3, server 11); W1 gate green; 2 pre-existing root clippy.toml warnings. Reviews: 0 Critical/High/Medium confirmed (1 Medium rejected by verifier); 8 Low, all fixed. No security-reviewer ran. Deviations: verify::file_matches returns VerifyError (Mismatch or Io); start_on added; hyper used directly, axum declared but unused. Open: drop_after header flush is timing-based (150 ms grace); no regression tests for the ctrl_c fallback or the accept backoff. TEST_EVIDENCE S0.5; SECURITY_REVIEW S0.5. |
| S0.6 | Storage | Planned | |
| S0.7 | IPC transport | Planned | |
| S0.8 | Agent skeleton | Planned | |
| S0.9 | CLI skeleton | Planned | |
| S0.10 | Desktop minimal shell | Planned | |
| S0.11 | Phase 0 gate | Planned | |

## Phase 1

| Slice | Title | Status | Evidence / references |
|---|---|---|---|
| S1.1 | URL policy, naming, state machine | Planned | |
| S1.2 | HTTP range semantics, redirect policy | Planned | |
| S1.3 | Retry policy, token bucket, estimator | Planned | |
| S1.4 | Fixture adversarial scenarios | Planned | |
| S1.5 | Platform filesystem primitives | Planned | |
| S1.6 | Mark-of-the-Web | Planned | |
| S1.7 | Single-stream vertical slice | Planned | |
| S1.8 | Agent bootstrap | Planned | |
| S1.9 | Resume, checkpoints, first proof (M1) | Planned | |
| S1.10 | Desktop real task list | Planned | |
| S1.11 | Segmented transfer | Planned | |
| S1.12 | Retries, redirects, network hardening | Planned | |
| S1.13 | Admission, budgets, bandwidth caps | Planned | |
| S1.14 | Recovery hardening, sampling, secret canary | Planned | |
| S1.15 | Phase 1 gate | Planned | |

## Deferred and partial items

These are seeded from `PHASE_0_1_PLAN.md` section 9 and stay as listed there until a slice changes them.
