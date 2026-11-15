# Security Review Log

This log records the reviews that actually ran: the reviewer, the scope, each finding with its severity, and how it was resolved (a fix plus a regression test, or a recorded blocker). The threat model and invariants are in `THREAT_MODEL.md`. Nothing here claims the product is "100% secure". It records what has been checked so far and what hasn't.

| Date | Slice | Reviewer | Critical | High | Medium | Low | Open Critical/High |
|---|---|---|---|---|---|---|---|
| 2026-11-15 | S0.1 | ecc:security-reviewer (supply chain) | 0 | 0 | 0 | 3 | 0 |
| 2026-11-15 | S0.1 | ecc:rust-reviewer | 0 | 0 | 0 | 1 | 0 |
| 2026-11-15 | S0.1 | ecc:code-reviewer | 0 | 0 | 0 | 2 | 0 |
| 2026-11-15 | S0.2 | ecc:security-reviewer (threat-model sign-off) | 0 | 0 | 3 (verifier: 1 Medium, 2 Low) | 3 | 0 |
| 2026-11-15 | S0.2 | ecc:architect (consistency) | 0 | 0 | 6 | 9 | 0 |

Process: each Medium or higher finding went to an independent verifier asked to refute it, and was fixed only if confirmed. Low findings were passed to the fixer unverified.

## Findings

### S0.1 (all Low)
- `reqwest::get` and `tokio::fs::OpenOptions::append` were not in clippy `disallowed-methods`. **Fixed**; clippy still passes, so the paths resolve.
- The `crates/*` members glob would enroll any new directory automatically. **Fixed** with an explicit members list.
- `unsafe_code = deny` can be overridden locally. No change now; S0.4 must scope `allow(unsafe_code)` to the platform FFI modules only (checked by `check.ps1 unsafe-only-in-platform`).
- `ring` appears in `Cargo.lock` through optional edges, but `cargo tree -i ring` shows it is not built. The deny.toml at the gate must reason over the resolved feature graph.
- `tls_smoke` only asserts that `build()` returns Ok. Deeper behaviour is covered by S1.7/S1.12 network tests.
- `CLAUDE.md` was missing. **Created** by the coordinator.

### S0.2 (8 Medium confirmed, all fixed in docs)
- `cargo deny` was documented without `--locked`, wrongly claiming the flag doesn't exist. Fixed: `cargo deny --locked check ...` (flag placement verified with cargo-deny 0.20.2 `--help`).
- The plan still described worktree/merge gating after the main-checkout deviation. Fixed.
- ARCHITECTURE contradicted Appendix A in four places: 401 routing, retry_waiting admission, reconcile of `finalizing`, and a download using a response-only error code. All four were fixed. The last two raised PCR-01 and PCR-02, which the coordinator decided and applied (PLAN 11.1).
- IPC-04 had controls with no named test: engine busy, queue cap, one request at a time. Tests are now named in S0.7/S1.7 (commitments, not yet run).
- Remaining sub-controls without tests were named or marked residual. `reject_remote_clients` has no readback API, so it stays UNVERIFIED (THREAT_MODEL section 8).

## Not run

- AgentShield: not installed.
- cargo-audit: not installed (cargo-deny advisories used instead).
