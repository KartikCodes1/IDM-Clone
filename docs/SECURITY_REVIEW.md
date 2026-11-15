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
| 2026-11-15 | S0.4b | ecc:rust-reviewer | 0 | 0 | 0 | 0 | 0 |
| 2026-11-15 | S0.4b | ecc:security-reviewer | 0 | 0 | 0 | 1 | 0 |
| 2026-11-15 | S0.6 | ecc:rust-reviewer | 0 | 0 | 1 | 4 | 0 |
| 2026-11-15 | S0.6 | ecc:security-reviewer | 0 | 0 | 0 (1 Medium rejected by verifier) | 6 | 0 |
| 2026-11-15 | S0.6 | ecc:code-reviewer | 0 | 0 | 1 | 5 | 0 |
| 2026-11-15 | S0.7 | ecc:rust-reviewer | 0 | 0 | 0 | 6 | 0 |
| 2026-11-15 | S0.7 | ecc:security-reviewer | 0 | 0 | 0 (1 Medium rejected by verifier) | 4 | 0 |
| 2026-11-15 | S0.7 | ecc:code-reviewer | 0 | 0 | 0 | 3 | 0 |
| 2026-11-15 | S0.7 | ecc:silent-failure-hunter | 0 | 0 | 0 (1 Medium rejected by verifier) | 4 | 0 |
| 2026-11-15 | S1.1 | ecc:rust-reviewer | 0 | 0 | 0 | 4 | 0 |
| 2026-11-15 | S1.1 | ecc:security-reviewer | 0 | 0 | 0 | 2 | 0 |
| 2026-11-15 | S1.1 | ecc:code-reviewer | 0 | 0 | 0 | 2 | 0 |
| 2026-11-15 | S1.2 | ecc:rust-reviewer | 0 | 0 | 0 | 2 | 0 |
| 2026-11-15 | S1.2 | ecc:security-reviewer | 0 | 0 | 0 | 0 | 0 |
| 2026-11-15 | S1.2 | ecc:code-reviewer | 0 | 0 | 0 | 0 | 0 |
| 2026-11-15 | S0.8 | ecc:rust-reviewer | 0 | 0 | 0 | 5 | 0 |
| 2026-11-15 | S0.8 | ecc:security-reviewer | 0 | 0 | 0 | 2 | 0 |
| 2026-11-15 | S0.8 | ecc:code-reviewer | 0 | 0 | 0 | 2 | 0 |
| 2026-11-15 | S0.8 | ecc:silent-failure-hunter | 0 | 0 | 0 (2 Medium rejected by verifier) | 5 | 0 |
| 2026-11-15 | S1.4 | ecc:rust-reviewer | 0 | 0 | 0 | 5 | 0 |
| 2026-11-15 | S1.4 | ecc:code-reviewer | 0 | 0 | 0 | 2 | 0 |
| 2026-11-15 | S1.3 | ecc:rust-reviewer | 0 | 0 | 1 (fixed) | 2 | 0 |
| 2026-11-15 | S1.3 | ecc:code-reviewer | 0 | 0 | 0 | 0 | 0 |
| 2026-11-15 | S0.10 | ecc:typescript-reviewer | 0 | 0 | 0 | 1 | 0 |
| 2026-11-15 | S0.10 | ecc:react-reviewer | 0 | 0 | 0 | 4 | 0 |
| 2026-11-15 | S0.10 | ecc:security-reviewer | 0 | 0 | 0 | 0 | 0 |
| 2026-11-15 | S0.10 | ecc:rust-reviewer | 0 | 0 | 1 (not fixed, open) | 5 | 0 |
| 2026-11-15 | S0.10 | ecc:code-reviewer | 0 | 0 | 0 | 0 | 0 |

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

### S0.4b Platform pipe returns a tokio NamedPipeServer (PCR-04) (0 Critical/High/Medium; 1 Low)

Confirmed (verified) findings: none.

Other Low findings:
- S0.4b-security-reviewer-1 (Low): the security tests did not prove the replacement-instance DACL or the byte-mode flag (IPC-01, IPC-02). **Fixed**: `replacement_instances_carry_the_same_dacl_and_byte_mode` reads back the DACL, label and PipeMode::Byte from the first and a non-first instance. PIPE_REJECT_REMOTE_CLIENTS still cannot be read back; open.

The ecc:rust-reviewer found no defects (count 0). Both reviewers read the code only and did not run cargo.

Verifier-rejected: none.
Re-review: none recorded.

### S0.6 Storage (0 Critical/High; 2 Medium, 15 Low)

Confirmed (verified) findings:
- S0.6-rust-reviewer-1 (Medium): save_checkpoint trusted a caller-supplied bytes_committed. **Fixed**: the segment sum is checked and InvalidValue returned before any write; test `checkpoint_rejects_bytes_committed_mismatch`.
- S0.6-code-reviewer-13 (Medium): `concurrent_immediate_writers_never_busy` could not detect the failure it is named for. **Fixed**: unit test `db::tests::begin_write_takes_the_write_lock_up_front`, which fails against a deferred-BEGIN mutant (confirmed by mutation run); the concurrency test is now multi_thread.
- S0.6-rust-reviewer-2 (Low): replace_plan and save_checkpoint persist plans that load() later rejects. **Fixed** by documentation only: tiling is validated on load (ARCHITECTURE 7.2); no write-time check added.
- S0.6-rust-reviewer-3 (Low): SettingKey docs promise ranges that put() does not enforce. **Fixed** by documentation: storage checks only size; range checks belong to the protocol layer or the caller.
- S0.6-security-reviewer-6 (Low): generic transition() can set Completed, bypassing the finalizing-only complete() path. **Fixed**: returns InvalidValue{state} for Completed; test inside `complete_set_finalize_target_and_delete`.
- S0.6-security-reviewer-7 (Low): complete() accepts an unvalidated checksum. **Fixed in part**: checksum must be 64 lowercase hex characters. The length_verified and checksum_expected parts were refuted by the verifier and not applied.

Other Low findings:
- S0.6-rust-reviewer-4: concurrency test cannot exercise the BEGIN IMMEDIATE guarantee. **Fixed** by the same test as code-reviewer-13.
- S0.6-rust-reviewer-5: events.append for an unknown download surfaced as a generic Database error. **Fixed**: maps to NotFound{id}; test `event_append_for_unknown_download_is_not_found`. Length caps not added.
- S0.6-security-reviewer-9: plaintext URL not zeroed (DB-04). **Not fixed**: needs zeroize (new dependency, not allowed) or a hand-rolled wipe. Open.
- S0.6-security-reviewer-10: no bound on segment count; no check of bytes_committed against segments and total_size. **Fixed in part**: bytes_committed vs segments enforced in save_checkpoint. Segment-count cap and total_size CHECK not added (schema change).
- S0.6-security-reviewer-11: set_finalize_target had no state guard. **Fixed**: requires state = 'finalizing'; returns Conflict or NotFound. Test added.
- S0.6-security-reviewer-12: url_display stored without a redaction check. **Not fixed** (LOW, optional): redaction is the caller's contract. Not tracked elsewhere.
- S0.6-code-reviewer-14: plaintext URL not zeroed (ADR-012, ARCHITECTURE 7.5). **Not fixed**: same as security-reviewer-9; open for docs tracking.
- S0.6-code-reviewer-15: unused dependencies (tracing; tokio as a normal dependency). **Not fixed**: removing them changes Cargo.lock, which is forbidden in this wave. Open issue.
- S0.6-code-reviewer-16: several persisted columns have no write path. **Not fixed** (LOW, optional): consuming slices will add setters.
- S0.6-code-reviewer-17: awkward retention conversion and redundant validation. **Fixed**.
- S0.6-code-reviewer-18: save_checkpoint trusted caller-supplied bytes_committed. **Fixed**: same fix as rust-reviewer-1.

Verifier-rejected:
- S0.6-security-reviewer-8 (Medium): no size bounds on plaintext columns or the URL before sealing. Rejected by verifier.

Severity note: the rust-reviewer summary text said three Medium and two Low; the verified records list one Medium (rust-reviewer-1) and four Low. The table uses the verified records.
Re-review: none recorded.

### S0.7 IPC transport (0 Critical/High/Medium confirmed; 17 Low)

Confirmed (verified) findings:
- S0.7-rust-reviewer-1 (Low): handler calls are not cancellable, so shutdown can hang. **No change needed** in code: a doc note on `Server::serve` says the caller must bound it with a timeout (S0.8 applies the bound).
- S0.7-security-reviewer-7 (Low): handler call not bounded by shutdown or a timeout. **No change needed**: same as rust-reviewer-1.
- S0.7-silent-failure-hunter-16 (Low): abnormal close reasons logged only at debug. **Fixed**: read errors logged at warn with the error kind; write errors and write timeouts logged at warn.
- S0.7-silent-failure-hunter-17 (Low): frame-level error responses with request_id 0 silently skipped by the client. **Fixed**: client logs a warn for an ignored response that matches no in-flight request; an id-0 failure is not returned as the answer.

Other Low findings:
- S0.7-rust-reviewer-2: panics in connection tasks silently discarded. **Fixed**: `log_join` logs JoinError at error level.
- S0.7-rust-reviewer-3: biased select lets a constant event stream starve request reads. **Not fixed**: ordering is bounded and changing it could regress existing tests.
- S0.7-rust-reviewer-4: 16 KiB buffer per read_message; next_event ignored `broken`. **Fixed** for `broken` (returns Closed); per-call allocation left as is.
- S0.7-rust-reviewer-5: wrong result variant on handshake reported as Closed. **Fixed**: IpcError::UnexpectedResult; client marked broken.
- S0.7-rust-reviewer-6: needless clone in `comparable`. **Fixed**.
- S0.7-security-reviewer-9: `low_integrity_client_denied` passed silently when setup failed, and its final assertion was vacuous. **Fixed**: setup failure panics; vacuous assertion and unused helper removed.
- S0.7-security-reviewer-10: client queues unbounded events; pipe_name not constrained to a local pipe. **Fixed in part**: `Client::connect` rejects non-`\\.\pipe\` names (InvalidPipeName, regression test). Client event queue cap not implemented.
- S0.7-security-reviewer-11: write timeout is not a canonical constant. **Not fixed**: needs a canonical WRITE_TIMEOUT in A.9 and forgedm-protocol (outside slice).
- S0.7-code-reviewer-12: replacement creation retries on ERROR_PIPE_BUSY, which deviates from plan wording. **No change needed** in code: the spec wording is for the coordinator to amend.
- S0.7-code-reviewer-13: Timeouts.write is a tunable that is not in Appendix A.9. **Not fixed**: same as security-reviewer-11.
- S0.7-code-reviewer-14: MAX_ACTIVE_CONNECTIONS not tied to PIPE_MAX_INSTANCES. **Fixed**: compile-time assert in lib.rs.
- S0.7-silent-failure-hunter-18: unframeable event dropped with no log. **Fixed**: logged at error level before it is skipped.
- S0.7-silent-failure-hunter-19: unexpected result variant in handshake() mapped to Closed. **Fixed**: same as rust-reviewer-5.

Verifier-rejected:
- S0.7-security-reviewer-8 (Medium): biased select lets a busy subscription starve inbound reads and trip the frame timeout. Rejected by verifier.
- S0.7-silent-failure-hunter-15 (Medium): connection task panics (JoinError) silently discarded. Rejected by verifier; same subject as rust-reviewer-2, which is fixed at Low.

Re-review: none recorded.

### S1.1 URL policy, naming, state machine (0 Critical/High/Medium; 8 Low)

Confirmed (verified) findings:
- S1.1-security-reviewer-5 (Low): sanitiser left C1 controls (U+0080 to U+009F) and invisible or bidi code points in file names (FS-01). **Fixed**: all Cc characters plus U+00AD, U+061C, U+2028/2029 and U+2060 to U+2069 removed; test `c1_and_invisible_format_characters_removed`.

Other Low findings:
- S1.1-rust-reviewer-1: parse_filename unconditionally skipped the first `;` segment. **Fixed**; test `cd_filename_without_disposition_type_is_found`.
- S1.1-rust-reviewer-2: backslash escape handling in split_unquoted broke legacy Windows-path values. **Fixed**; test `cd_legacy_backslash_paths_keep_later_params`. The reviewer's example (a trailing escaped quote) is valid under both rules, so the test covers legacy backslash paths and an escaped backslash before the closing quote.
- S1.1-rust-reviewer-3: wildcard arms on the DownloadState business enum. **Fixed**: every variant listed, so a new state breaks the build; unused helper removed.
- S1.1-rust-reviewer-4: brittle hard-coded counts in the table test. **Fixed**: counts derived or replaced by a duplicate-row check.
- S1.1-security-reviewer-6: embedded_ipv4 missed IPv4-compatible and 6to4 forms, which classified as Public (NET-06). **Fixed**: both forms unwrapped; four new cases. Reserved ranges and multicast unchanged (open).
- S1.1-code-reviewer-7: an unusable filename* suppressed a usable plain filename. **Fixed**: an empty or whitespace-only filename* falls back to filename. A filename* decoding to `..` still wins and is rejected by sanitisation (quality gap only).
- S1.1-code-reviewer-8: test comment cited the wrong ARCHITECTURE section. **Fixed**: now cites ARCHITECTURE 6.3 step 1.

Verifier-rejected: none.
Re-review: none recorded.

### S1.2 HTTP range semantics and redirect policy (0 Critical/High/Medium; 2 Low)

Confirmed (verified) findings: none.

Other Low findings:
- S1.2-rust-reviewer-1: a 416 without If-Range and with a stored strong validator always yields RemoteChanged. **Fixed** by documenting the If-Range invariant on `RangedRequest::if_range_sent` and pinning it with `_416_without_if_range_and_strong_validator_is_remote_changed`. The Unknown case for 416 was not changed, because the existing test asserts the current behaviour.
- S1.2-rust-reviewer-2: a 206 total mismatch is strict when the stored total is unknown. **No change needed**: pinned by the existing test `rejects_206_total_mismatch`; a lenient trial failed that test and was reverted.

The ecc:security-reviewer and ecc:code-reviewer reported no findings.

Verifier-rejected: none.
Re-review: none recorded.

### S0.8 Agent skeleton (0 Critical/High/Medium after verification; 14 Low)

Confirmed (verified) findings:
- S0.8-rust-reviewer-1 (Low): cancelling the token directly never moved the hub to ShuttingDown. **Fixed**: removed the is_cancelled early return from request_shutdown; SetState is a no-op when the state is unchanged, so duplicate shutting_down events are avoided too. Regression test set_state_to_the_current_state_is_a_no_op.
- S0.8-silent-failure-hunter-12 (Low): failures before logging init are invisible in release builds. **No change needed**: the pre-log startup order is required by ARCHITECTURE 2.3; main.rs documents that these failures are reported by exit code only.

Other Low findings:
- S0.8-rust-reviewer-2: storage not closed on startup errors after it was opened. **Fixed**: start() closes storage when start_hub, the handler install or set_state(Ready) fails.
- S0.8-rust-reviewer-3: subscribe reply timeout can leave a live sink registered in the hub. **Fixed**: record() prunes closed sinks on every event; regression test closed_subscribers_are_pruned_on_the_next_record.
- S0.8-rust-reviewer-4: Ctrl+C handler failure is silent. **Fixed**: the Err branch logs a warning.
- S0.8-rust-reviewer-5: wildcard match on the DownloadState business enum. **Fixed**: Interrupted and Paused are matched explicitly; any other target state returns StorageError::InvalidValue and logs an error.
- S0.8-security-reviewer-6: explicit --data-dir is not checked for a user-only ACL (SEC-01 and DB-04 assume one). **Not fixed**: needs a Windows DACL helper in the platform crate or a THREAT_MODEL update, both outside the slice. Open.
- S0.8-security-reviewer-7: unrestricted --log-level can enable dependency debug/trace logging that bypasses the SEC-01 call-site redaction discipline. **Not fixed**: scoping the filter changes the CLI contract; left for S1.14 secret_canary_absent or a decision. Open.
- S0.8-code-reviewer-8: unused anyhow and serde dependencies and an unused feature flag. **Not fixed**: removing the dependencies makes --locked fail (Cargo.lock would change), so that edit was reverted. The fault-injection feature was removed from Cargo.toml, which does not touch the lock. Open for the lock owner.
- S0.8-code-reviewer-9: failures before logging starts are never recorded in release builds. **No change needed**: same as silent-failure-hunter-12; by design per ARCHITECTURE 2.3, documented in main.rs.
- S0.8-silent-failure-hunter-13: ctrl_c() registration error silently ignored. **Fixed**: same change as rust-reviewer-4.
- S0.8-silent-failure-hunter-14: event-sink drops and publish drops are not logged. **Fixed**: dropped subscribers are logged at debug level with a count; the lost-Subscribe-reply case is covered by pruning of closed sinks.
- S0.8-silent-failure-hunter-15: unknown stored error code and negative timestamps silently coerced. **Fixed**: warnings added for both; the fallbacks are kept.
- S0.8-silent-failure-hunter-16: drain timeout and capped loading degrade without a count. **Fixed**: the drain warning now includes the timeout. Not added: an open-connection count, and a truncated flag for the load cap.

Verifier-rejected:
- S0.8-silent-failure-hunter-10 (reported Medium): shutdown settle failure is only logged; the agent still exits 0.
- S0.8-silent-failure-hunter-11 (reported Medium): pipe squatting (exit 4) is not logged as a forgedm::security event.

Re-review: none recorded.

### S1.4 Fixture adversarial scenarios (0 Critical/High/Medium after verification; 7 Low)

Confirmed (verified) findings:
- S1.4-rust-reviewer-1 (Low): unchecked u64 arithmetic panics on request input (bad_cr). **Fixed**: content_range in response.rs uses saturating arithmetic; test bad_cr_with_huge_size_does_not_panic covers u64::MAX with every bad_cr variant and asserts a 206.
- S1.4-rust-reviewer-2 (Low): overflow in the multipart total length (and in assemble). **Fixed**: the total is a saturating fold checked against MAX_BODY_BYTES before assemble, and parts are capped at 256; tests multipart_overlapping_ranges_on_huge_size_do_not_panic and tests/memory.rs.

Other Low findings:
- S1.4-rust-reviewer-3: fixed 300 ms sleep, then an exact assertion that slots are released. **Fixed**: log_tracks_max_concurrency polls current_concurrency() against a 5 s deadline; the slow_from assertion is loosened to a 3 s prefix bound.
- S1.4-rust-reviewer-4: raw lie_cl path silently ignored other flags and bypassed route() rules. **Fixed**: lie_cl rejects slow, drop_after, stall_after, max_conns, status, redirect, signed, auth and multipart with 400, and HEAD follows the head flag; tests lie_cl_rejects_flags_it_cannot_honour and lie_cl_head_honours_head_flag; limits documented in the raw.rs module doc.
- S1.4-rust-reviewer-5: malformed or unparsable request on the raw path was dropped without any response. **Fixed**: a 400 is sent before the socket closes; test raw_malformed_request_gets_400.
- S1.4-code-reviewer-6: if_range_matches was about 54 lines. **Fixed**: split into strong_etag_matches and last_modified_matches.
- S1.4-code-reviewer-7: ttl_requests accepted without redirect=signed_expiring or signed, and ttl_requests=0 allowed. **Fixed by documenting the behaviour**: ttl_requests=0 forces an immediately expired signed link, locked in by ttl_requests_zero_forces_an_expired_signed_link (expects 403).

Verifier-rejected: none.
Re-review: none recorded.

### S1.3 Retry policy, token bucket, estimator (0 Critical/High; 1 Medium; 2 Low)

Confirmed (verified) findings:
- S1.3-rust-reviewer-1 (Medium): HostState::on_throttled panicked on an oversized cooldown. **Fixed**: the cooldown is clamped to RETRY_AFTER_WAIT_MAX (3600 s) before it is added to the Instant; regression test on_throttled_with_oversized_cooldown_does_not_panic calls it with Duration::MAX and Duration::from_secs(u64::MAX).

Other Low findings:
- S1.3-rust-reviewer-2: refused-at-open does not set cooldown_until, unlike ARCHITECTURE 4.11. **No change needed**: Appendix A.9 only requires halving the limit, and the existing test encodes the no-cooldown reading. Recorded as an open issue in TEST_EVIDENCE for a docs alignment.
- S1.3-rust-reviewer-3: a regressed byte counter lowers the baseline and double-counts later bytes. **Fixed**: Sampler::advance and Estimator::sample keep the highest byte count seen; regression test regressed_counter_does_not_double_count_recovered_bytes (failed before the fix at 72.29 > 0, passes now).

Verifier-rejected: none.
Re-review: none recorded.

### S0.10 Desktop minimal shell (0 Critical/High; 1 Medium, open; 10 Low)

Confirmed (verified) findings:
- S0.10-react-reviewer-2 (Low): every row's progressbar had the same accessible name. **Fixed**: each has aria-label "Progress of <display_name>" (or "... unknown").
- S0.10-rust-reviewer-6 (Low): the agent locator was resolved once at startup and a failure was cached permanently. **Fixed**: AgentTarget resolves the locator lazily behind a Mutex and caches only Ok values; regression test commands::tests::locator_failure_is_not_cached_and_success_is.
- S0.10-rust-reviewer-7 (Medium): tracing events go nowhere because no subscriber is installed. **Not fixed, open**: a subscriber needs a new dependency, which the plan rules forbid. The misleading comments in error.rs were corrected and the missing sink is recorded as an open issue.

Other Low findings:
- S0.10-typescript-reviewer-1: a transient poll failure replaces the ready snapshot with an error, and the in-flight result is not cancelled on unmount. **Fixed in part**: an active ref guards setSnapshot after unmount; keeping the last good snapshot was not done.
- S0.10-react-reviewer-3: aria-live on a region that is mounted with its content. **Not fixed** (LOW, optional).
- S0.10-react-reviewer-4: rows are not memoized, so every 2 s poll re-renders the whole list. **Not fixed** (LOW, optional).
- S0.10-react-reviewer-5: one transient poll failure replaces the ready view with an error or offline screen. **Fixed in part**: only the unmount guard was added; the flicker remains.
- S0.10-rust-reviewer-8: startup error is discarded. **Not fixed**: needs a log sink (open issue).
- S0.10-rust-reviewer-9: wildcard arm on IpcError hides new variants. **Not fixed**.
- S0.10-rust-reviewer-10: blocking subprocess and SystemRoot environment trust. **Not fixed**: the SID lookup now runs lazily on the first command, but it is still a blocking call.
- S0.10-rust-reviewer-11: handler and manifest checks parse source text with string splitting. **Not fixed**.

Verifier-rejected: none.
Re-review: none recorded.

## Not run

## Not run

- AgentShield: not installed.
- cargo-audit: not installed (cargo-deny advisories used instead).
