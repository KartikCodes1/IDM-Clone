# Implementation Status

Updated 2026-11-15. Statuses: **Implemented** (code exists), **Tested** (tests ran and passed; evidence in `TEST_EVIDENCE.md`), **Partial**, **Deferred**, **Blocked**, **Planned** (not started). A slice counts as done only when it is Tested, its reviews are clean, and it has zero open Critical/High findings.

## Phase 0

| Slice | Title | Status | Evidence / references |
|---|---|---|---|
| S0.1 | Workspace scaffold, toolchain pin, dependency lock | **Tested** (uncommitted) | 8 packages, 326 locked crates, `tls_smoke` 1 passed; security/rust/code reviews: 0 Critical/High/Medium, 6 Low (2 fixed, 4 no change). TEST_EVIDENCE "S0.1". Deviation: `[workspace.lints.cargo] unused_dependencies = "allow"` until the Phase 1 gate (deps are pre-declared by design). |
| S0.2 | Architecture, threat model, development docs | **Implemented** (docs; uncommitted) | GATE 1 approved 2026-11-15. Docs accepted, `DEVELOPMENT.md` written. Security-reviewer and architect sign-off review: 0 Critical/High, 8 Medium confirmed by a verifier, all fixed. PCR-01/02 decided. SECURITY_REVIEW "S0.2". |
| S0.3 | Protocol v1 | Planned | |
| S0.4 | Platform security primitives | Planned | |
| S0.5 | Fixture server baseline | Planned | |
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
