# Security Review Log

This log records the reviews that actually ran: the reviewer, the scope, each finding with its severity, and how it was resolved (a fix plus a regression test, or a recorded blocker). The threat model and invariants are in `THREAT_MODEL.md`. Nothing here claims the product is "100% secure". It records what has been checked so far and what hasn't.

| Date | Slice | Reviewer | Critical | High | Medium | Low | Open Critical/High |
|---|---|---|---|---|---|---|---|
| 2026-11-15 | S0.1 | ecc:security-reviewer (supply chain) | 0 | 0 | 0 | 3 | 0 |
| 2026-11-15 | S0.1 | ecc:rust-reviewer | 0 | 0 | 0 | 1 | 0 |
| 2026-11-15 | S0.1 | ecc:code-reviewer | 0 | 0 | 0 | 2 | 0 |
| 2026-11-15 | S0.2 | ecc:security-reviewer (threat-model sign-off) | 0 | 0 | 3 (verifier: 1 Medium, 2 Low) | 3 | 0 |
| 2026-11-15 | S0.2 | ecc:architect (consistency) | 0 | 0 | 6 | 9 | 0 |
| 2026-11-15 | S0.3 | ecc:rust-reviewer | 0 | 0 | 0 | 3 | 0 |
| 2026-11-15 | S0.3 | ecc:security-reviewer | 0 | 0 | 0 | 4 | 0 |
| 2026-11-15 | S0.3 | ecc:code-reviewer | 0 | 0 | 0 | 5 | 0 |
| 2026-11-15 | S0.4 | ecc:rust-reviewer | 0 | 0 | 1 | 3 | 0 |
| 2026-11-15 | S0.4 | ecc:security-reviewer | 0 | 0 | 0 | 3 | 0 |
| 2026-11-15 | S0.4 | ecc:code-reviewer | 0 | 0 | 1 | 2 | 0 |
| 2026-11-15 | S0.5 | ecc:rust-reviewer | 0 | 0 | 0 | 5 | 0 |
| 2026-11-15 | S0.5 | ecc:code-reviewer | 0 | 0 | 0 (1 Medium rejected by verifier) | 3 | 0 |

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

### S0.3 Protocol v1 (0 Critical/High/Medium after verification; 12 Low)

Confirmed (verified) findings:
- S0.3-rust-reviewer-1 (Low): duplicate keys in the payload were silently last-wins. **Fixed**: `StrictValue` in `src/strict_json.rs` rejects duplicate keys at any depth; regression test `duplicate_payload_keys_rejected`.
- S0.3-rust-reviewer-2 (Low): error classification depended on serde_json message text. **Fixed**: test `serde_error_wording_maps_to_stable_reasons`. The `invalid_length` case is still untested.
- S0.3-code-reviewer-8 (Low): Appendix A extended without a protocol change request. **Not fixed**: needs a PCR or an Appendix A update; open.

Other Low findings:
- S0.3-rust-reviewer-3: oversize encode reported a fake length. **Fixed**: doc comment states the length saturates at u32::MAX; no behaviour change.
- S0.3-security-reviewer-4: derived Debug on EnqueueUrlRequest and Request could leak URL credentials. **Fixed**: manual Debug prints the URL as `<redacted>`; test `debug_of_enqueue_request_redacts_url_credentials`.
- S0.3-security-reviewer-5: duplicate payload keys (same as rust-reviewer-1). **Fixed**.
- S0.3-security-reviewer-6: the absolute-path check accepts any backslash-prefixed path, including device and UNC namespaces, and target_dir has no length bound. **Not fixed**: Plan C28 defers this to Phase 1 agent validation; open.
- S0.3-security-reviewer-7: free-form String reason fields could carry unredacted text. **No change needed**: reasons are written only from static strings; test `error_reasons_never_echo_input_values`.
- S0.3-code-reviewer-9: Unknown serialized as "Unknown" while as_str returned "unknown". **Fixed**: serde rename; test `unknown_variant_serializes_as_its_as_str`.
- S0.3-code-reviewer-10: duplicate payload keys (same as rust-reviewer-1). **Fixed**.
- S0.3-code-reviewer-11: DownloadDetail uses serde flatten despite the no-flatten design rule. **Not fixed**: result side only and disclosed; needs a plan or Appendix A carve-out.
- S0.3-code-reviewer-12: the chunked proptest rarely reaches body accumulation. **Not fixed** (optional); deterministic frame tests cover those paths.

Verifier-rejected: none.
Severity note: the rust-reviewer summary text said two Medium; the verified records list rust-reviewer-1 and -2 as Low, so the table counts them as Low. The slice deviation note says 7 of 11 findings were fixed; the outcomes list has 12 entries (7 fixed, 1 no change, 4 not fixed).
Re-review: none recorded.

### S0.4 Platform security primitives (0 Critical/High; 2 Medium, 8 Low)

Confirmed (verified) findings:
- S0.4-rust-reviewer-1 (Medium): verbatim_path silently altered unpaired surrogates. **Fixed**: components are pushed as OsString unchanged; test `verbatim_path_preserves_unpaired_surrogates`.
- S0.4-code-reviewer-8 (Medium): create_pipe_instance deviates from the specified tokio API and cannot be adopted by tokio without new unsafe. **Open, not changed**: tokio is not a declared dependency, and adding it changes Cargo.lock, which is outside this slice's authority. Coordinator decision needed before S0.7.
- S0.4-security-reviewer-5 (Low): verbatim_path passes ADS colons, trailing dots and spaces, and device-style names through the `\\?\` bypass (FS-01, FS-03). **No change needed**: deliberate, because rejecting ':' breaks the MOTW Zone.Identifier writer; name safety belongs to the FS-01 sanitiser. Doc comment added; test `verbatim_path_passes_component_names_through_unvalidated`.

Other Low findings:
- S0.4-rust-reviewer-2: unwrap_or(0) hid a constant conversion failure in SECURITY_ATTRIBUTES.nLength. **Fixed**: const SECURITY_ATTRIBUTES_LEN with a compile-time assert.
- S0.4-rust-reviewer-3: ERROR_ACCESS_DENIED was mapped to NameInUse even when first is false. **Fixed**: mapped only when first is true; no dedicated non-first test.
- S0.4-rust-reviewer-4: weak assertions in the instance-cap test and in same_path. **Recorded as fixed, but only in part**: the cap test asserts ERROR_PIPE_BUSY (231); the same_path helper is unchanged.
- S0.4-security-reviewer-6: IPC ACL tests prove the descriptor text, not that foreign principals are denied (THREAT_MODEL 4.3). **No change**: out of scope this round; cross-user denial is verified by DACL readback only; open.
- S0.4-security-reviewer-7: peer identity is resolved by PID after accept, so PID reuse is possible; the image check is path-only. **No change**: optional; residual risk, open.
- S0.4-code-reviewer-9: verbatim_path used to_string_lossy, turning unpaired surrogates into U+FFFD. **Fixed**: same fix as rust-reviewer-1.
- S0.4-code-reviewer-10: the DACL readback test did not do an exact match and ignored the SACL flags. **Fixed**: asserts SACL flags `AI` (auto-inherited, added by Windows); ACE equality assertions kept.

Verifier-rejected: none.
Re-review: none recorded.

### S0.5 Fixture server baseline (0 Critical/High/Medium confirmed; 8 Low)

Confirmed (verified) findings:
- S0.5-rust-reviewer-1 (Low): the accept loop spun on persistent accept errors. **Fixed**: 50 ms backoff unless the error is ConnectionAborted or ConnectionReset (retried at once). Accept errors are still not logged, since the crate has no logging facility.

Other Low findings:
- S0.5-rust-reviewer-2: drop_after relied on a fixed 50 ms sleep. **Fixed**: grace raised to 150 ms and the timing dependence documented; still timing-based.
- S0.5-rust-reviewer-3: stale `# Errors` doc on the infallible build. **Fixed**: heading removed.
- S0.5-rust-reviewer-4: invalid header values were silently dropped by set. **Fixed**: lm validated in Flags::from_pairs (FlagError); unit test covers %0d%0a and %ff.
- S0.5-rust-reviewer-5: no unit tests for range.rs, flags.rs or percent_decode. **Fixed**: unit tables added.
- S0.5-code-reviewer-7: serve exited if the ctrl_c handler failed to install. **Fixed**: serve awaits pending() and keeps serving.
- S0.5-code-reviewer-8: accept errors retried in a tight loop. **Fixed**: same backoff as rust-reviewer-1.
- S0.5-code-reviewer-9: no dedicated unit tests for range and flags edge cases. **Fixed**: range::evaluate edge-case table and flags tests.

Verifier-rejected:
- S0.5-code-reviewer-6 (reported Medium): unused declared dependencies (axum, anyhow, http-body-util). Rejected by the verifier. The implementer's deviation note says axum is declared but not needed.

Severity note: the rust-reviewer summary text said one Medium and four Low; the verified records list 5 Low and no Medium. No security-reviewer ran for S0.5.
Re-review: none recorded.

## Not run

- AgentShield: not installed.
- cargo-audit: not installed (cargo-deny advisories used instead).
