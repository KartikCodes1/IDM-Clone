Status: ACCEPTED at GATE 1 (2026-11-15). Implementation is in progress; docs/IMPLEMENTATION_STATUS.md records what is actually built and tested.

# ForgeDM Threat Model (Phase 0/1 depth, whole-product breadth)

| Field | Value |
|---|---|
| Date | 2026-11-15 |
| Sources | Master spec sections 3, 4, 5, 10, 11, 12; execution prompt section 3; research bundle (probes on Windows 11 build 26300; tokio, reqwest, windows-rs, RFC 9110) |
| Canonical names | `docs/PHASE_0_1_PLAN.md` Appendix A; design in `docs/ARCHITECTURE.md` |
| Claim | No claim of complete security. Every control below is a commitment (design and test plan), not a statement that it exists. Implemented and tested status is recorded only in `docs/IMPLEMENTATION_STATUS.md` and `docs/TEST_EVIDENCE.md`. **UNVERIFIED** items must be proven by a test before they are relied on. |

## 0. Conventions
- **P01**: Phase 0/1 control with an owning slice and named tests. **PL**: later-phase control, listed so interfaces stay open; the Phase 1 residual is stated.
- **Tests** are cited as `S<slice> test_name` exactly as named in PHASE_0_1_PLAN; static checks as `check.ps1 :: <name>` (PLAN section 8.3).
- L = likelihood, I = impact (Low/Med/High). Severity uses ECC levels (CRITICAL, HIGH, MEDIUM, LOW).
- Threat-ID prefixes: NET, FS, IPC, DB, SEC, UI, SUP, PLAT, EXT, PL.

**GATE 1 items that change this model** (PLAN sections 1-2): C2 probe (HEAD hint + mandatory `bytes=0-0` GET), C4 pipe DACL without SYSTEM, C11 per-value DPAPI (no AEAD dependency), C17 non-identity Content-Encoding rejected, C25 literal-only private-destination policy in Phase 1, C26 client full-path server check, C28 no approved-roots list in Phase 1, D1 prebuilt NASM objects, D4 supply-chain scanners, D8 sanitized MOTW URLs.

---

## 1. Assets

| ID | Asset | C | I | A | Notes |
|---|---|---|---|---|---|
| A1 | Completed downloaded files | Med | **High** | Med | Bytes must equal the remote file; the user's trust is the product |
| A2 | `.forgedm.part` files | Low | **High** | Med | Silent corruption becomes a "completed" file; on-disk length is never trusted |
| A3 | User filesystem outside the destination | High | **High** | High | Never created, overwritten, deleted or followed through links |
| A4 | SQLite DB (sealed URLs with signed tokens, history) | **High** | High | Med | `%LOCALAPPDATA%\ForgeDM\forgedm.db` |
| A5 | DPAPI master keys (user credentials) protecting sealed columns | **High** | High | Med | OS-managed |
| A6 | IPC channel (named pipe) | Med | **High** | Med | Control plane for every client |
| A7 | Agent process (runs as the user) | n/a | **High** | High | File-write capability and network reach |
| A8 | Logs, diagnostics, exports | Med (leak vector) | Low | Low | |
| A9 | Agent network position (loopback, LAN, intranet) | Med | n/a | n/a | SSRF target set |
| A10 | Installer and updater chain | Med | **High** | Med | PL (Phase 2) |
| A11 | Browser-extension and native-host channel | Med | High | Med | PL (Phase 3) |
| A12 | Build supply chain (crates, npm, toolchain) | Med | **High** | Med | |
| A13 | The user's attention to OS warnings (MOTW, SmartScreen) | n/a | **High** | n/a | Stripping MOTW is a security failure |

## 2. Trust boundaries

```
                      INTERNET (untrusted)                    LAN / localhost services (sensitive)
   [remote HTTP(S) servers, CDNs, redirect chains]                     ^
        | status, headers, bodies, redirects, CD, CR, ETag, Retry-After | (SSRF risk)
======== | ================ TB1: network (TLS verified) ===============|==================
        v                                                              |
 +----------------- forgedm-agent (per user, medium IL) ------------------------------+
 | core: probe -> range_decision -> segment_table -> writer thread -> verify/finalize |
 |   url_policy | http::redirect | naming | retry/hosts | rate_limit                  |
 | storage (sqlx/SQLite, DPAPI-sealed columns)    protocol (framed, versioned)        |
 | platform (only unsafe: DACL/SID/peer, DPAPI, fs, MOTW, known folders)              |
 +----^------------------------------------------------------------------^------------+
      | TB2: named pipe (explicit DACL, first instance, reject remote,    | TB4: filesystem
      |      peer SID both ways, client checks server image path)         |  (target_dir vs rest of disk)
  +---+-----------+-----------------+------------------+          +-----v------------------------+
  | forgedm (CLI) | Tauri shell     | native host (PL) |          | %LOCALAPPDATA%\ForgeDM (DB,  |
  |               | Rust side       | TB3 browser<->host|         |  logs, run\) user-only       |
  +---------------+--------^--------+------------------+          | target_dir: .part + finals   |
                           | TB5: Tauri invoke (AppManifest ACL)   | (shared with other software) |
                  +--------v----------------+                      +------------------------------+
                  | WebView2 (React, CSP)   |   Same-user malware: inside the OS trust boundary,
                  +-------------------------+   outside ForgeDM's defensible boundary (section 8).
```

| TB | Boundary | Concern |
|---|---|---|
| TB1 | Agent <-> network | Everything inbound is hostile; outbound can be abused (SSRF, amplification) |
| TB2 | Agent <-> local IPC clients | Forged clients, pipe squatting, malformed frames, exhaustion |
| TB3 | Browser <-> native host (PL) | Web pages and extensions reaching the agent |
| TB4 | Agent <-> filesystem | Traversal, links, races, other processes touching `.part` files |
| TB5 | Webview <-> Tauri Rust | XSS leading to command invocation; engine data rendered as HTML |
| TB6 | Build and dependencies | Compromised crates, npm packages, toolchain, installers |

## 3. Attacker-controlled inputs

| Input | Source | Trust |
|---|---|---|
| HTTP status and headers (Content-Length, Content-Range, Content-Disposition, Content-Type, Content-Encoding, ETag, Last-Modified, Date, Retry-After, Location, Set-Cookie, Accept-Ranges, Transfer-Encoding) | remote server or MITM | none |
| Body bytes, timing, stalls, mid-stream drops | remote server | none |
| Redirect chains (schemes, hosts, loops, downgrades, private targets, signed-URL expiry) | remote server | none |
| URLs, target dirs, file names from CLI/desktop | local caller | syntax/policy untrusted; intent trusted only after peer verification |
| IPC frames (length, JSON, version, action, ids) | any pipe client | untrusted until the peer is verified; payloads always validated |
| Proxy env vars and HKCU Internet Settings | parent process, same-user software | semi-trusted (NET-10) |
| DB rows and `.part` contents | disk (other processes, crashes, tampering) | never trusted for resume beyond `[start, committed)` with file-id match |
| Directory contents under `target_dir` (planted links, ADS, same-name files) | other software or users | untrusted |
| Downloaded content | remote server | hostile; never opened, parsed or executed |
| Dependency sources and binaries | registries | untrusted until locked and reviewed |
| Engine-originated strings shown in UI (names, URLs, errors) | remote server via agent | untrusted as HTML |

---

## 4. Threat register

Format: **ID title (severity; L/I) [scope]** then Controls / Owner / Tests / Residual. Owner = crate::module (slice).

### 4.1 Network and protocol (TB1)

**NET-01 Invalid or lying ranges corrupt the file (CRITICAL; High/High) [P01]**
- Controls: `http::range_decision::decide` runs before any write (ARCHITECTURE 4.6): accept only a single-part identity 206 `bytes s-x/T` with x <= requested end, equal T, consistent Content-Length; body capped at x+1-s; a 200 to a ranged request is never written at offset s; claim-under-lock segment table so no write leaves its segment's claimed window.
- Owner: core `http::range_decision` (S1.2), `transfer::segment_table` (S1.11), `writer` (S1.7).
- Tests: S1.2 `accepts_short_206_with_continuation`, `rejects_206_wrong_start`, `rejects_206_end_beyond_request`, `rejects_206_total_mismatch`, `rejects_multipart_206`, `rejects_content_encoded_206`, `rejects_206_cl_mismatch`, `rejects_u64_overflow_accepts_above_u32`, `proptest_accept_window_within_request`; S1.9 `200_on_resume_never_written_at_offset`; S1.11 `bad_content_range_writes_nothing`, `short_206_continues`, `ranges_ignored_mid_download_no_misplaced_writes`, `stress_shrunk_worker_never_writes_past_end`, `proptest_table_invariants`.
- Residual: a server that consistently serves different bytes under the same length and validators is undetectable without a user-supplied checksum; SHA-256 is always recorded and `resume`/`length_verified` labels stay honest.

**NET-02 Remote content changes between segments or sessions (HIGH; Med/High) [P01]**
- Controls: store T plus strong ETag or strong Last-Modified (Date >= LM + 1 s); `If-Range` only with a strong validator; every resume re-probes the original URL and compares identity; a 200 to an If-Range request -> `remote_changed`, all workers stop; weak/no validator -> 64 KiB sample verification, labelled `best_effort`.
- Owner: core `http::validators` (S1.2), `resume` (S1.9, S1.14).
- Tests: S1.2 `strong_compare_requires_both_strong`, `if_range_never_weak`, `lm_strength_rule`, `200_to_ranged_with_if_range_is_remote_changed`, `416_other_total_remote_changed`; S1.9 `resume_rejected_when_etag_changed_between_runs`, `weak_or_no_validator_restarts_from_zero`; S1.14 `sampling_detects_change_without_strong_validator`.
- Residual: with weak/no validators a change outside the sampled 64 KiB can mix versions; only a user-supplied hash detects it.

**NET-03 MITM or TLS downgrade (CRITICAL; Med/High) [P01]**
- Controls: reqwest rustls + platform verifier (Windows store and enterprise roots); no reachable bypass, custom-root, key-log, preconfigured-TLS or early-data API (clippy `disallowed-methods` with old and new names, `check.ps1 :: no-tls-bypass`); TLS errors are fatal and never retried; https -> http redirects blocked.
- Owner: core `client` (S1.7, S1.12); workspace config (S0.1).
- Tests: S0.1 `client_builds_with_configured_tls_provider`; S1.12 `tls_handshake_failure_fatal_not_retried`, `tls_untrusted_cert_fatal` (D9 only), `https_to_http_rejected`; S1.2 `redirect_downgrade_rejected`; `check.ps1 :: no-tls-bypass`.
- Residual: user- or enterprise-installed roots (incl. TLS inspection) are trusted by the OS store and therefore by ForgeDM.

**NET-04 Redirect abuse: loops, downgrade, header leakage, expired signed URLs (HIGH; High/High) [P01]**
- Controls: manual redirects through the pure `http::redirect` policy: 10 hops, visited-set loop detection, http/https only, downgrade blocked, private-destination rule on every hop (NET-06); no Referer; Phase 1 never sends Authorization, Cookie or custom headers; origin-bound scoping (original origin only, not previous hop) is specified for later phases and is covered by the S1.2 test `origin_bound_headers_only_to_original_origin` once that slice lands; Phase 1 sends no such headers; expiry -> one re-resolve from the original URL with identity check, then `needs_url_refresh` (max 3/hour).
- Owner: core `http::redirect` (S1.2), `client`/`probe` (S1.7, S1.12), `resume` (S1.14).
- Tests: S1.2 `redirect_hop_limit_10`, `redirect_loop_detected`, `redirect_downgrade_rejected`, `redirect_non_http_scheme_rejected`, `redirect_location_resolved_relative_and_capped_8kib`, `origin_bound_headers_only_to_original_origin`; S1.7 `redirect_chain_followed_with_pure_policy`; S1.12 `redirect_chain_final_url_recorded_sealed`, `loop_and_over_limit_rejected`, `https_to_http_rejected`, `no_referer_sent`, `no_auth_or_cookie_seen_cross_host`; S1.14 `signed_expiring_reresolved_once`, `reresolve_capped_3_per_hour`.
- Residual: a legitimate same-origin redirect to attacker content on that origin is by design.

**NET-05 Unsafe URL scheme or syntax (HIGH; Med/High) [P01]**
- Controls: parse with `url`; http/https only; userinfo rejected; <= 8 KiB; control characters rejected; fragment stripped; host required.
- Owner: core `url_policy` (S1.1); protocol `validate` (S0.3).
- Tests: S1.1 `scheme_allowlist`, `userinfo_rejected`, `overlong_and_control_chars_rejected`, `fragment_stripped_on_validation`; S0.3 `validate_rejects_each_limit`; S1.7 `invalid_urls_rejected_with_codes`.
- Residual: IDN lookalike domains need UI support (PL, Phase 2).

**NET-06 SSRF against loopback, LAN, link-local or metadata addresses (HIGH; CRITICAL once the extension exists) [P01 partial + PL]**
- P01 controls: Phase 1 URLs come only from verified same-user CLI/desktop clients; `classify_host` works on the parsed `url::Host` (all WHATWG IPv4 forms, IPv4-mapped IPv6, unspecified, loopback, RFC1918, CGNAT, link-local incl. 169.254.169.254, ULA, `localhost`, `*.localhost`); a user-entered local URL is allowed (the user typed it); a redirect hop to a local/private literal from a non-local origin is blocked (`redirect_blocked`).
- PL controls (Phase 3, required before the browser extension ships): classify resolved addresses for DNS names using a pinned custom resolver (`ClientBuilder::dns_resolver`; `resolve()` overrides bypass it, so tests need an injectable resolver); extension-sourced requests blocked from private destinations unless the user allows the host.
- Owner: core `url_policy` (S1.1), `http::redirect` (S1.2).
- Tests: S1.1 `local_destination_classification`; S1.2 `redirect_public_to_local_rejected_unless_origin_local`; S1.12 `public_to_loopback_literal_rejected`.
- Residual (Phase 1): a public DNS name resolving to a private or loopback address (incl. DNS rebinding) is not blocked; acceptable while only the local user can enqueue. With a proxy configured, destination classification is the proxy's. Reserved and multicast literals (224/4, 240/4, 255.255.255.255, 192.0.0.0/24, 198.18/15, IPv6 multicast) classify as `Public` in Phase 1, because `HostClass` has no variant for them (S1.1, W2). They are revisited with the Phase 3 DNS-resolved policy.

**NET-07 Decompression and size bombs, header floods (HIGH; Med/Med) [P01]**
- Controls: no reqwest compression features plus `no_gzip/no_brotli/no_deflate/no_zstd` plus `Accept-Encoding: identity`; any non-identity Content-Encoding rejected (Inv-07); `http1_max_headers(128)` bounds header count (byte-size enforcement UNVERIFIED, covered by the raw huge-header test); body never exceeds Content-Range; free-space precheck before the first write; memory bounded independent of file size.
- Owner: core `client` (S1.7), `http::probe_classify` (S1.2), `engine::admission` (S1.13), `writer` (S1.11).
- Tests: S1.7 `gzip_response_rejected`, `identity_header_sent`; S1.2 `rejects_content_encoded_206`, `content_encoded_200_rejected`; `check.ps1 :: no-reqwest-compression`; S1.12 `raw_protocol_violations_typed_errors_no_panic`; S1.13 `free_space_precheck_disk_full`; S1.11 `writer_backpressure_bounded_queue`; S1.15 working-set measurement.
- Residual: a hostile server can send a huge legitimate-looking file up to free space; a user maximum-size setting is deferred.

**NET-08 Slow or stalled streams exhaust resources (MEDIUM; High/Med) [P01]**
- Controls: connect 15 s and per-read 30 s timeouts (never a total body timeout); bounded active downloads (3), per-host (8) and global (32) connections.
- Owner: core `client` (S1.7), `engine::budgets`/`admission` (S1.13).
- Tests: S1.12 `stall_times_out_and_retries`; S1.12 `connect_timeout_15s_blackhole` (`#[ignore]`, slow tier); S1.13 `max_three_active`, `per_host_budget_no_503_with_max_conns_8`, `global_budget`.
- Residual: a server trickling just above the stall threshold holds a slot; low-speed reconnect is deferred (Phase 5).

**NET-09 Retry storms and amplification (MEDIUM; Med/Med) [P01]**
- Controls: full-jitter backoff with injectable clock/RNG; Retry-After against the response `Date` with clamps (<= 120 s inline, <= 1 h `retry_waiting`, longer fails); per-host cooldown pausing all segments plus AIMD; 150 ms connection stagger; HTTP/1.1 only so connection counts are real; retry budgets; fatal classes never retried.
- Owner: core `retry` (S1.3), `engine::hosts` (S1.12).
- Tests: S1.3 `full_jitter_bounds_seeded`, `retry_after_seconds_and_http_date_vs_response_date`, `retry_after_clamped_and_beyond_cap_wait_until`, `retry_after_over_1h_is_rate_limited_failure`, `aimd_halves_on_429_recovers`, `budget_exhaust_and_reset_after_progress`; S1.12 `transient_5xx_backoff_within_jitter_bounds`, `429_cooldown_pauses_all_segments_no_storm`, `attempt_budget_limits_request_count`, `401_needs_credentials_single_request`, `404_410_one_reresolve_then_needs_url_refresh`.
- Residual: aggregate load across many hosts is bounded only by the global budget.

**NET-10 Proxy environment or system proxy hijack (MEDIUM; Low-Med/High) [P01]**
- Controls: agent `--proxy system|none`; `ipc::bootstrap` removes `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` (both cases) when spawning, so routing does not depend on which client started the agent; `GetAgentStatus.proxy_source` reports `environment`, `system` or `none`; TLS still verified end to end through CONNECT; all tests use `none`.
- Owner: ipc `bootstrap` (S1.8), agent `config` (S0.8), core `client` (S1.7).
- Tests: S1.8 `bootstrapped_agent_does_not_inherit_proxy_env`; S0.8 `status_reports_pid_version_and_proxy_source`.
- Residual: an agent started manually from a shell with proxy variables honours them (reported as `environment`); the HKCU proxy keys are writable by same-user software (section 8); plain-http downloads are visible to any proxy. PAC/WPAD unsupported.

**NET-11 Credential and cookie leakage (HIGH; Med/High) [P01 narrow + PL]**
- P01 controls: Phase 1 never sends credentials, cookies, Authorization or custom headers; userinfo URLs rejected; 401/407 -> `needs_credentials` after a single request; the CLI reads URLs from stdin (`-`) so signed tokens stay out of argv.
- PL controls (Phase 2+ credentials flow): sealed request options, origin-bound attachment (ARCHITECTURE 4.9), https-only by default with explicit confirmation for http, `Sensitive<T>` wrappers.
- Owner: core `url_policy` (S1.1), `http::redirect` (S1.2), `client` (S1.12); cli (S1.7).
- Tests: S1.1 `userinfo_rejected`; S1.2 `origin_bound_headers_only_to_original_origin`; S1.12 `401_needs_credentials_single_request`, `no_auth_or_cookie_seen_cross_host`; S1.7 `url_from_stdin_keeps_secret_out_of_argv`.
- Residual: signed URL tokens live in agent memory while it runs.

### 4.2 Filesystem (TB4)

**FS-01 Malicious filename: traversal, device names, ADS, Unicode tricks (CRITICAL; High/High) [P01]**
- Controls: in-house Content-Disposition parser and Windows sanitizer (ARCHITECTURE 4.14): `filename*` over `filename`, final segment only, forbidden and control characters removed, `:` always removed, trailing dots/spaces trimmed, bidi/format controls removed, conservative reserved-name union (incl. COM0/LPT0, CONIN$/CONOUT$, superscript COM/LPT), 233-unit cap with extension kept; recorded name equals on-disk name.
- Owner: core `naming` (S1.1).
- Tests: S1.1 `cd_prefers_filename_star_rfc6266`, `rfc5987_decode_and_invalid_fallback`, `cd_path_components_reduced_to_basename`, `ads_colon_never_survives`, `reserved_names_union_rejected`, `trailing_dots_spaces_trimmed`, `bidi_controls_removed`, `empty_dot_dotdot_fallback`, `length_capped_at_233_preserving_extension`, `cd_name_255_units_fits_part_suffix`, `proptest_sanitized_always_safe_and_idempotent`; S1.7 `untrusted_cd_safe_name_inside_target`.
- Residual: Unicode lookalikes and double extensions still deceive humans; UI warning in Phase 2.

**FS-02 Symlink/junction swaps and overwrite races in the destination (HIGH; Med/High) [P01]**
- Controls: `.part` opened with `create_new` (adds FILE_FLAG_OPEN_REPARSE_POINT, so planted links fail); `ensure_within` on the handle's final path; name-surrogate reparse points below `target_dir` rejected (cloud placeholders allowed); no-replace `MoveFileExW` with each candidate persisted; `std::fs::rename` banned; 5/32 retried with bounded backoff; other processes get read-share only (`FILE_SHARE_READ`); a path-escape failure after `.part` creation deletes our just-created file and fails `path_escape` with a `forgedm::security` event (PLAN PCR-02); only recorded files with matching id are ever deleted.
- Owner: platform `fs` (S1.5), core `finalize` (S1.7).
- Tests: S1.5 `create_new_fails_on_planted_symlink` (NOT RUN if symlink privilege unavailable), `junction_escape_detected`, `name_surrogate_detection`, `no_replace_collision_already_exists`, `collision_suffix_picks_next_free_existing_untouched`, `finalize_move_reports_each_candidate_before_attempt`, `sharing_violation_retried_then_succeeds`, `part_share_mode_read_only_for_others`; S1.7 `existing_target_never_overwritten`; clippy `disallowed-methods` (`std::fs::rename`).
- Residual: directory-chain TOCTOU (full fix needs `NtCreateFile` with RootDirectory) is out of Phase 1 scope; a same-user attacker swapping directory components mid-run can redirect writes within that user's own permissions.

**FS-03 Destination outside the user's intent; uninstall deleting downloads (HIGH; Low/High) [P01 + PL]**
- P01 controls: default from `FOLDERID_Downloads` (never env vars, never under app data); `target_dir` validated (absolute, existing directory, local fixed/removable volume, not a device path, UNC deferred, canonicalised); `ensure_within` after create; delete only the recorded `.part` with matching `part_file_id`; completed files never deleted in Phase 1; CLI `remove --delete-partial` requires `--yes`.
- PL controls: approved-roots list before any external source (extension) can propose a destination (Phase 3; the reserved `native_host` role cannot set `target_dir` at all); installer/uninstaller must never remove downloads (Phase 2 test).
- Owner: platform `known_folders` (S0.4), agent `handlers::enqueue` and core `finalize` (S1.7), agent `handlers::control` (S1.9).
- Tests: S0.4 `known_folders_downloads_and_localappdata_absolute_dirs`; S1.7 `invalid_destinations_rejected_with_codes`, `untrusted_cd_safe_name_inside_target`; S1.9 `cancel_keeps_part_remove_deletes_only_part`, `remove_refuses_replaced_part_file`, `cli_remove_delete_partial_requires_yes`.
- Residual: the local user may choose any writable directory, including sensitive ones; that is their intent.

**FS-04 Archive extraction (zip-slip)** - not applicable: ForgeDM does not extract archives. No extraction feature (incl. Phase 4 media outputs) is added without a model update.

**FS-05 Malicious executable content and Mark-of-the-Web loss (HIGH; High/High) [P01]**
- Controls: no code path opens, previews or executes downloads; Zone.Identifier (ZoneId=3, sanitized `scheme://host[:port]/path`, D8) written on the `.part` before rename so the final name is never visible unmarked; IAE afterwards on a dedicated STA thread with 60 s timeout and poison fallback to the direct stream; failures visible (`motw_status`, `failed(blocked_by_antivirus | blocked_by_policy)`); FAT/exFAT shows `unsupported_fs`.
- Owner: platform `motw` (S1.6), core `finalize` (S1.7).
- Tests: S1.6 `sanitizer_strips_query_fragment_userinfo`, `non_http_or_overlong_about_internet`, `crlf_rejected`, `direct_ads_readback_zoneid3_hosturl_without_query`, `zero_byte_uses_direct_write`, `iae_timeout_poisons_worker_falls_back_to_direct`, `iae_from_sta_writes_zone_identifier` (ignored, AV-dependent); S1.7 `motw_applied_to_completed_file`; S1.14 `zone_identifier_present_at_finalizing_before_rename`, `zone_identifier_on_all_completed_files`.
- Residual: MOTW is advisory (software that ignores it, or a user unblocking the file, defeats it); IAE with `\\?\` paths, SmartScreen use of HostUrl and policy handling of the direct write are UNVERIFIED.

### 4.3 IPC (TB2)

**IPC-01 Forged client: other user, low integrity, remote (CRITICAL; Med/High) [P01]**
- Controls: explicit DACL `D:P(D;;GA;;;NU)(A;;GA;;;<user-sid>)S:(ML;;NWNR;;;ME)` (deny first; a NULL descriptor would grant Everyone read and is forbidden); SID validated before SDDL formatting; `reject_remote_clients` set on every instance (by construction; no readback API exists, so the flag's effect is UNVERIFIED, section 8 item 13); AppContainer denied (no AC SID, which the DACL readback test forbids); caller SID check before reading any frame, fail closed; bounded framing (1 MiB, length checked before allocation); handshake first; strict `deny_unknown_fields` and exhaustive actions; no TCP/HTTP listener.
- Owner: platform `sid`, `pipe`, `peer` (S0.4); ipc `server`, `identity` (S0.7); protocol (S0.3).
- Tests: S0.4 `pipe_dacl_readback_from_live_handle_is_deny_nu_allow_user_me_label_only`, `sid_validation_rejects_sddl_injection`, `peer_check_fails_closed_for_pid_4`; S0.7 `identity_decision_rejects_other_sid`, `low_integrity_client_denied` (UNVERIFIED method; NOT RUN if setup fails), `non_handshake_first_frame_closes`, `oversized_frame_closes_connection`, `garbage_client_then_next_client_served`, `version_mismatch_structured_error`; S0.3 `oversized_length_rejected_before_reading_body`, `proptest_decoder_never_panics_on_arbitrary_chunks`, `unknown_action_and_unknown_field_rejected`; `windows-smoke.ps1 :: agent-has-no-tcp-listener`.
- Residual: same-user medium-IL processes can connect and issue legal actions (section 8). Cross-user rejection beyond the descriptor readback needs a second account (manual). The remote-reject flag has no test (section 8 item 13).

**IPC-02 Pipe squatting and server impersonation (CRITICAL; Med/High) [P01]**
- Controls: lock file gate; first instance created with `first_pipe_instance(true)` (3 s retry, then security event + exit 4, never retried without the flag); capacity invariant `PIPE_MAX_INSTANCES = 17` with replacement-before-handoff so the name never disappears while serving; any later creation failure is fatal; clients verify server SID and full canonical image path (explicit expected path, case-insensitive) before the first byte; SQOS identification kept; pipe type/mode mismatch is not relied on (verified not to hold on build 26300).
- Owner: platform `pipe`, `instance_lock`, `peer` (S0.4); ipc `server`, `client` (S0.7); agent `app` (S0.8).
- Tests: S0.4 `second_first_instance_is_name_in_use`, `instance_lock_excludes_second_and_releases_on_drop`, `peer_check_rejects_unexpected_image`; S0.7 `squatted_name_server_start_fails_closed`, `pipe_name_never_vanishes_under_churn`, `wrong_server_image_rejected_and_server_receives_zero_frames`; S0.8 `squatted_pipe_exits_4_and_logs_security_event`, `second_agent_same_data_dir_exits_3`.
- Residual: cross-user squatting is a denial of service (agent exits 4 with a clear error); a random per-start pipe suffix is future hardening. PID reuse between connect and check is mitigated mainly by the DACL.

**IPC-03 Confused deputy through the Tauri shell (HIGH; Med/High) [P01]**
- Controls: custom commands ACL-gated through `AppManifest::new().commands(..)` (otherwise not ACL-checked); capability `main-window` grants only the app's commands; no core or plugin permissions; typed commands only, no generic frame forwarding; bounded request timeout.
- Owner: desktop `src-tauri` (S0.10, S1.10).
- Tests: S0.10 `config_grants_only_app_commands_and_strict_csp`, `manifest_matches_registered_commands` (updated in S1.10); S0.7 `client_request_times_out_after_10s` (the bounded request timeout).
- Residual: each Phase 2 command widens the surface; the manifest test stays in CI.

**IPC-04 IPC resource exhaustion (MEDIUM; Med/Med) [P01]**
- Controls: 16 active connections + 1 listening; handshake 5 s; frame body 10 s; idle 60 s unless subscribed; one request at a time per connection; outbound queue 256 with lag -> `ResyncRequired`; engine back-pressure -> `busy`; queue cap 10,000.
- Owner: ipc `server`, `subscribe` (S0.7); agent `events_hub` (S0.8).
- Tests: S0.7 `handshake_timeout_closes_connection`, `partial_frame_times_out`, `idle_connection_closed_after_timeout`, `seventeenth_client_waits_then_served`, `subscribe_seq_ordered_and_resync_on_lag`, `concurrent_clients_get_own_responses`, `pipelined_requests_answered_in_order_one_at_a_time` (S0.7; one request processed at a time per connection), `engine_busy_maps_to_busy` (S1.7; `busy` on a full engine channel or no reply within 5 s), `enqueue_over_queue_cap_limit_exceeded` (S1.7; queue cap 10,000 -> `limit_exceeded`).
- Residual: a same-user process can hold all 16 connections by subscribing (DoS from inside the boundary).

### 4.4 Storage (DB)

**DB-01 SQL injection (HIGH; Low/High) [P01]**
- Controls: runtime `query(..).bind(..)` with `&'static str` SQL only; `AssertSqlSafe` banned; writes via `BEGIN IMMEDIATE`; foreign keys on.
- Owner: storage (S0.6).
- Tests: S0.6 `sql_metacharacters_inert`; `check.ps1 :: no-assert-sql-safe`.
- Residual: none identified for Phase 1.

**DB-02 DB tampering or corruption causing a false "complete" (HIGH; Med/High) [P01]**
- Controls: DUR-3 flush-before-commit; WAL + `synchronous=FULL`; CHECK constraints plus Rust segment-tiling validation at load; agent-only forward migrations; `*.sql eol=lf`; a corrupt DB yields a typed error and exit 5 with the file preserved, never deleted; completion requires length and checksum (Inv-06).
- Owner: storage (S0.6), core `checkpoint` (S1.9), `recovery` (S1.14).
- Tests: S0.6 `segment_committed_outside_bounds_rejected`, `checkpoint_batch_is_atomic_on_mid_failure`, `corrupt_db_typed_error_file_bytes_unchanged`, `migration_files_have_no_crlf`, `unknown_state_string_rejected_on_load`; S0.8 `corrupt_db_exits_5_file_preserved`; S1.9 `commit_never_exceeds_last_flush`, `sync_data_failure_blocks_commit`, `resume_after_agent_kill_identical_sha256`, `416_at_end_after_crash_is_complete`; S1.14 `corrupt_segment_rows_safe_restart`, `crash_matrix_each_fault_point_recovers_exact_sha`, `multi_segment_force_kill_x3_no_corruption_no_inflation`.
- Residual: a same-user writer to the DB can steer offsets inside the recorded `.part` only (it could edit the files directly anyway). Power-loss durability is untested; kill tests do not prove FlushFileBuffers (ARCHITECTURE 5.4).

**DB-03 Stale, substituted or foreign partial file (HIGH; Med/High) [P01]**
- Controls: `part_file_id` recorded at `create_new`; resume reopen checks id, not-a-reparse-point and length >= max(committed); reconcile and Remove act only on id match; a same-name or same-size foreign file is never adopted as completed; NTFS keeps the id across the same-volume rename.
- Owner: platform `fs` (S1.5); core `finalize` (S1.7), `recovery` (S1.14); agent `handlers::control` (S1.9).
- Tests: S1.5 `part_file_id_survives_same_volume_rename`; S1.9 `remove_refuses_replaced_part_file`; S1.14 `reconcile_never_adopts_foreign_same_size_file`, `reconcile_finalizing_both_branches`, `missing_part_restarts`, `replaced_or_truncated_part_restarts`.
- Residual: byte substitution inside the committed range by the same user is undetectable without per-segment hashes (proposed later).

**DB-04 Sensitive values at rest (HIGH; Med/High) [P01]**
- Controls: original and final URLs stored only as per-value DPAPI CurrentUser blobs with entropy `"ForgeDM/v1/<column>/<download_id>"` (binds value to row and column); redacted `url_display` in plaintext; `CRYPTPROTECT_UI_FORBIDDEN`, no prompt struct, never LOCAL_MACHINE; plaintext zeroed; unseal failure -> `secrets_unavailable`; no new dependency (AES-GCM envelope rejected, C11).
- Owner: platform `dpapi` (S0.4), agent `sealer` (S0.8), storage (S0.6).
- Tests: S0.4 `dpapi_roundtrip_and_ciphertext_hides_plaintext`, `dpapi_tampered_blob_fails`, `dpapi_wrong_entropy_fails`; S0.6 `sealed_columns_raw_db_and_wal_bytes_lack_canary`, `wrong_row_entropy_fails`, `unseal_failure_is_typed_crypto_error`; S1.14 `secret_canary_absent`.
- Residual: DPAPI stops other users and offline theft, not same-user malware (which can call CryptUnprotectData); an admin password reset makes values unrecoverable (handled as `secrets_unavailable`). Plaintext URLs live in agent memory for the lifetime of a download, because requests need them. Platform DPAPI buffers are zeroed, but storage does not zero caller-owned `String`s after sealing, because there is no `zeroize` dependency (accepted residual, W2 S0.6).

### 4.5 Privacy and secrets

**SEC-01 Secret leakage through logs, events, diagnostics, UI (HIGH; High/Med-High) [P01]**
- Controls: URLs leave the engine only as `RedactedUrl` (`scheme://host[:port]/path`); `SecretUrl`/`Sensitive<T>` print `[redacted]` and do not Serialize; reqwest errors stripped with `Error::without_url()`; clippy `print_stdout`/`print_stderr`/`dbg_macro`; no telemetry or crash upload; logs in the user-only data directory.
- Owner: core `url_policy` (S1.1), `client` (S1.12); agent `logging` (S0.8).
- Tests: S1.1 `redaction_strips_query_fragment_userinfo`; S1.12 `error_messages_contain_no_url_query`; S1.14 `secret_canary_absent` (query, fragment and redirect-Location tokens absent from DB/WAL/SHM bytes, logs, Zone.Identifier, CLI output, IPC frames).
- Residual: tokens embedded in the URL path (e.g. `/dl/<token>/f.zip`) survive redaction in `url_display`, logs and Zone.Identifier HostUrl; redacting high-entropy path segments is proposed later. New log call sites need review.

**SEC-02 Secrets on the command line (MEDIUM; Med/Med) [P01]**
- Controls: `forgedm add -` reads the URL from stdin; no credential flags exist (credentials deferred); CLI help warns about shell history.
- Owner: cli (S1.7).
- Tests: S1.7 `url_from_stdin_keeps_secret_out_of_argv`.
- Residual: URLs typed as arguments remain in shell history and the process command line.

### 4.6 UI, supply chain, platform

**UI-01 XSS or engine data rendered as HTML (HIGH; Med/High) [P01]**
- Controls: React text rendering only; ESLint bans `dangerouslySetInnerHTML`, `eval`, `new Function`, `innerHTML`; CSP `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'; frame-ancestors 'none'`; `freezePrototype: true`; `withGlobalTauri: false`; asset protocol off; `assetsInlineLimit: 0`; runtime validation of every IPC payload; CSP checked on `tauri build --debug --no-bundle` (not applied under `tauri dev`).
- Owner: desktop (S0.10, S1.10).
- Tests: S0.10 `config_grants_only_app_commands_and_strict_csp`, `malicious_name_renders_as_text`, `parseAgentStatus_accepts_golden_rejects_malformed`; S1.10 `malformed_payload_error_state`; `check.ps1 :: eslint-bans`; S0.10 manual DevTools CSP record.
- Residual: Phase 2 libraries that inject `<style>` may force an evaluated CSP change.

**SUP-01 Dependency or toolchain compromise (HIGH; Low-Med/High) [P01]**
- Controls: committed lockfiles with `--locked` / `--frozen-lockfile`; exact pin `windows =0.62.2` only (C5); tauri 2.12.5 and tauri-build 2.7.4 locked by `Cargo.lock` (ARCHITECTURE 1.5 gives them as caret-default versions), with `tauri-versions-match` checking major.minor against `@tauri-apps/cli` and `api`; TypeScript `~6.0.3`; all other versions locked by `Cargo.lock` / `pnpm-lock.yaml`; pnpm `minimumReleaseAge: 1440`, `strictDepBuilds`, `engineStrict`, empty `onlyBuiltDependencies`, `packageManager` pinned to the installed pnpm; never add windows-core, rusqlite or unvetted/GPL code; D1 uses NASM objects shipped inside aws-lc-sys (pinned by Cargo.lock checksum) instead of installing NASM: a recorded trade-off; cargo-deny (advisories, licences, bans, sources) runs under D4; cargo-audit is not used (not installed; cargo-deny advisories replace it); if cargo-deny is not run, record NOT RUN; `pnpm audit --prod` always.
- Owner: workspace root (S0.1), desktop packages (S0.10), gate scripts (S0.11, S1.15).
- Tests: `check.ps1 :: lockfiles-committed`, `no-loose-versions`, `tauri-versions-match`; D4 tools.
- Residual: a compromised legitimate release is not caught until disclosed; delayed adoption only reduces exposure.

**SUP-02 Test fixture or fault injection shipped (HIGH; Low-Med/High) [P01]**
- Controls: `forgedm-fixture` is `publish = false` and only a dev-dependency; binds 127.0.0.1 only; agent `fault-injection` feature compiled out of release and read only with the feature.
- Owner: workspace root (S0.1), fixture (S0.5), agent `fault` (S1.14).
- Tests: S0.5 `binds_loopback_only`; `check.ps1 :: no-fixture-in-shipped`, `no-fault-injection-in-release`.
- Residual: none significant once the checks run in CI.

**PLAT-01 Unneeded privilege or elevation (HIGH; Low/High) [P01]**
- Controls: per-user agent; no services, drivers, `Global\` objects or elevation; NSIS `currentUser` and opt-in autostart in Phase 2.
- Tests: `check.ps1 :: manifest-asinvoker`; the whole suite runs non-admin.
- Residual: none.

**PLAT-02 Disk-full, removable-drive loss, locked files handled unsafely (MEDIUM; Med/Med) [P01]**
- Controls: disk-full and device-gone pause (not retry); free-space precheck and periodic recheck; FAT32 >= 4 GiB fails early; finalize lock retry with budget.
- Owner: platform `fs` (S1.5), core `engine` (S1.11, S1.13), `recovery` (S1.14).
- Tests: S1.5 `disk_full_codes_classified`, `sharing_violation_retried_then_succeeds`; S1.11 `fat32_over_4gib_fails_early_file_system_limit`; S1.13 `free_space_precheck_disk_full`; S1.14 `disk_full_injected_pauses_then_resumes`, `device_gone_injected_pauses`.
- Residual: a real full volume and a real removable-drive pull are manual tests.

**PLAT-03 NTFS valid-data-length stall on far-offset writes (LOW; availability) [P01]**
- Controls: sparse `.part` on NTFS (non-admin, verified); `SetFileValidData` never used (admin; exposes stale disk data).
- Tests: S1.5 `sparse_5gib_logical_small_allocation_attr_0x200`, `clear_sparse_after_full_write`; S1.11 `sparse_set_then_cleared`, `worker_writes_beyond_4gib_offsets_sparse`.
- Residual: fragmentation; disk-full can surface mid-download (handled as a pause).

**PLAT-04 Unsafe FFI misuse (HIGH if present; Med/High) [P01]**
- Controls: `unsafe` only in `forgedm-platform` FFI modules with `// SAFETY:` (clippy `undocumented_unsafe_blocks`); `forbid(unsafe_code)` elsewhere; `Owned<T>` RAII for HANDLE/HLOCAL; every return value checked (`EqualSid` Err = not equal); descriptor kept alive for the server lifetime; no impersonation in Phase 1; `windows` pinned `=0.62.2`.
- Owner: platform (S0.4, S1.5, S1.6).
- Tests: `check.ps1 :: unsafe-only-in-platform`; rust-reviewer unsafe audit on S0.4, S1.5, S1.6; the S0.4/S1.5/S1.6 test suites exercise every FFI path.
- Residual: handle leaks on rare fault paths.

### 4.7 Later-phase threats (interfaces preserved now)

| ID | Threat | Phase | Interface kept open |
|---|---|---|---|
| EXT-01 | Web page drives the native host to enqueue arbitrary URLs (SSRF, abuse) | 3 | `source` recorded from handshake `client_kind`; `native_host` reserved with restricted actions and no `target_dir`; DNS-resolved destination policy (NET-06 PL) |
| EXT-02 | Rogue extension or spoofed extension id | 3 | exact `allowed_origins`; native host validates type, size, origin; per-client-kind action allow-list in the agent |
| EXT-03 | Cookie/header copying from the browser | 3 | origin-bound header scoping already in `http::redirect`; opt-in per site |
| EXT-04 | Native host logging to stdout corrupts framing | 3 | logging init takes a sink |
| PL-01 | Installer/uninstaller deletes downloads or leaves keys | 2 | data in `%LOCALAPPDATA%\ForgeDM`, downloads never under app data; uninstall removes only keys ForgeDM created |
| PL-02 | yt-dlp/FFmpeg supply chain and RCE surface | 4 | no process spawn in core; future spawns use absolute vetted paths, argument arrays (no shell), checksum/provenance, explicit approval |
| PL-03 | Signing and update channel | 2 | no updater without a threat-model update |
| PL-04 | Tray/notification text from untrusted names | 2 | same text-only rule as UI-01 |

---

## 5. Security invariants (release blockers)

| # | Invariant | Enforced by |
|---|---|---|
| Inv-01 | No unvalidated write: a network byte is written only after `decide` accepts it and only inside its segment's claimed window | NET-01 tests; S1.11 `proptest_table_invariants` |
| Inv-02 | Persisted segments tile [0, T) without overlap; final length equals T | S0.6 `segment_committed_outside_bounds_rejected`; S1.11 `logged_ranges_disjoint_and_cover`; S1.14 `corrupt_segment_rows_safe_restart` |
| Inv-03 | No blind append, no length-as-progress: resume uses only DB committed offsets with a file-id match | S1.9 `pause_flush_commit_resume_uses_committed_offset_and_strong_if_range`; S1.14 `replaced_or_truncated_part_restarts` |
| Inv-04 | Flush before commit: committed <= flushed at all times | S1.9 `commit_never_exceeds_last_flush`, `sync_data_failure_blocks_commit` (kills do not prove this) |
| Inv-05 | Strong validators only in `If-Range`; changed validator or total stops the download | S1.2 `if_range_never_weak`, `lm_strength_rule`; S1.9 `resume_rejected_when_etag_changed_between_runs` |
| Inv-06 | No false completion: `completed` only with length = T (or honestly `length_verified = false`) and, if expected, SHA-256 of the on-disk bytes matches; mismatch -> `failed(checksum_mismatch)`, `.part` kept; foreign files never adopted | S1.7 `sha_match_completes_mismatch_fails_keeps_part`, `unknown_length_completes_length_unverified`, `drop_after_with_cl_is_incomplete_not_completed`; S1.9 `resumed_download_checksum_actual_matches_generator`; S1.14 `reconcile_never_adopts_foreign_same_size_file` |
| Inv-07 | No decoding: non-identity Content-Encoding rejected on 200 and 206 | S1.7 `gzip_response_rejected`; S1.2 `rejects_content_encoded_206`, `content_encoded_200_rejected`; `check.ps1 :: no-reqwest-compression` |
| Inv-08 | TLS always verified; no bypass or custom-root path | `check.ps1 :: no-tls-bypass`; clippy disallowed-methods; S1.12 `tls_handshake_failure_fatal_not_retried` |
| Inv-09 | Origin-bound credentials (none are sent in Phase 1; scoping ready for later) | S1.2 `origin_bound_headers_only_to_original_origin`; S1.12 `no_auth_or_cookie_seen_cross_host` |
| Inv-10 | Bounded redirects with policy on every hop (<= 10, no loops, no downgrade, http/https only, private-destination rule) | NET-04 and NET-06 tests |
| Inv-11 | Safe names only; recorded name equals on-disk name | FS-01 tests |
| Inv-12 | Created files stay under the validated `target_dir`; name-surrogate links never followed | S1.5 `junction_escape_detected`, `name_surrogate_detection`; S1.7 `untrusted_cd_safe_name_inside_target`. A path escape after `.part` creation fails `path_escape` with a `forgedm::security` event (PLAN PCR-02) |
| Inv-13 | No clobber; never delete a file ForgeDM did not create | S1.7 `existing_target_never_overwritten`; S1.5 `collision_suffix_picks_next_free_existing_untouched`; S1.9 `remove_refuses_replaced_part_file` |
| Inv-14 | Pipe is user-only (explicit DACL, first instance, remote rejected, not dropped while the agent serves: the replacement instance exists before handoff; a crash or failed replacement is not covered, IPC-02 residual); no TCP/HTTP listener | S0.4 DACL readback; S0.7 `pipe_name_never_vanishes_under_churn`; `windows-smoke.ps1 :: agent-has-no-tcp-listener` |
| Inv-15 | Clients send zero bytes before the server SID and full image path are verified | S0.7 `wrong_server_image_rejected_and_server_receives_zero_frames` |
| Inv-16 | Bounded IPC: frame size and time, handshake time, idle time, connections, queue depth; unknown actions/fields fail closed | S0.3 and S0.7 tests (IPC-01, IPC-04); queue cap: S1.7 `enqueue_over_queue_cap_limit_exceeded` |
| Inv-17 | Secrets never plaintext at rest or in logs/output (except documented path-token residual) | S0.6 `sealed_columns_raw_db_and_wal_bytes_lack_canary`; S1.14 `secret_canary_absent` |
| Inv-18 | No execution of downloaded data; process spawn only for the agent bootstrap | `check.ps1 :: command-new-allowlist` |
| Inv-19 | MOTW preserved on every completed non-empty NTFS file, sanitized HostUrl, present before the final name exists; failure visible | S1.6 tests; S1.14 `zone_identifier_present_at_finalizing_before_rename`, `zone_identifier_on_all_completed_files` |
| Inv-20 | Bounded memory and concurrency (independent of file size; active/per-host/global budgets) | S1.11 `writer_backpressure_bounded_queue`; S1.13 budget tests; S0.7 `seventeenth_client_waits_then_served`; S1.15 working-set measurement |
| Inv-21 | No retry storm: jitter, clamped Retry-After, host cooldown, budgets; fatal classes never retried | NET-09 tests |
| Inv-22 | Least privilege: `asInvoker`, no service/driver/`Global\`/elevation | `check.ps1 :: manifest-asinvoker` |
| Inv-23 | Webview reaches the engine only through AppManifest-allowed commands; strict CSP; no HTML injection | S0.10 `config_grants_only_app_commands_and_strict_csp`, `manifest_matches_registered_commands`, `malicious_name_renders_as_text`; `check.ps1 :: eslint-bans` |
| Inv-24 | Fixture and fault injection never ship | `check.ps1 :: no-fixture-in-shipped`, `no-fault-injection-in-release` |
| Inv-25 | Honest state: UI/CLI show only agent-reported state; progress from accepted/committed bytes; unknown sizes never shown as percent | S1.7 `get_download_true_bytes_midway`, `enqueue_to_completed_with_ordered_events`; S0.10 `App_unknown_size_shows_unknown_not_percent`; S1.10 `table_renders_true_values`; S1.14 `multi_segment_force_kill_x3_no_corruption_no_inflation`; S0.8 `no_event_lost_between_snapshot_and_subscribe` |

---

## 6. Test plan notes

- Every test above is a commitment of the slice that owns it (PHASE_0_1_PLAN sections 4-5). `TEST_EVIDENCE.md` records the real command and exit code, or NOT RUN with the reason.
- The fixture request log (method, Range, If-Range, Authorization/Cookie presence, connection id, peer port, concurrency high-water mark) lets tests assert invariants, not only outcomes.
- The first proof (S1.9 `resume_after_agent_kill_identical_sha256` and `phase1-proof.ps1`) must show resume actually happened (request-log offsets and served-byte bound), not just a matching hash.
- Coverage target >= 80% on protocol, core, storage is meaningful only with the adversarial tests present (execution prompt section 4); measured only with the D4 tooling (approved at GATE 1) actually run; otherwise NOT RUN.

**Proposed later (not committed to Phase 0/1):** cargo-fuzz targets (Content-Range, Content-Disposition, Retry-After, URL policy, frame decoder, sanitizer); loom model of claim/shrink (Phase 1 uses proptest); cross-account pipe test; real small-volume disk-full and removable-drive tests; OneDrive placeholder test; `NtCreateFile` relative-open hardening; per-segment rolling hashes and a user "verify" action; path-token redaction; random per-start pipe suffix; native-host framing fuzz and extension origin/SSRF suites (Phase 3); installer tests (Phase 2).

## 7. Responsibility map

| Module | Threats owned | Slice(s) |
|---|---|---|
| `forgedm-protocol` | IPC-01, IPC-04, Inv-16 | S0.3 |
| `forgedm-platform::{sid,pipe,peer,instance_lock}` | IPC-01, IPC-02, PLAT-04 | S0.4 |
| `forgedm-platform::{known_folders,dpapi,wide}` | FS-03, DB-04, PLAT-04 | S0.4 |
| `forgedm-platform::fs` | FS-02, DB-03, PLAT-02, PLAT-03 | S1.5 |
| `forgedm-platform::motw` | FS-05 | S1.6 |
| `forgedm-ipc::{server,client,identity,subscribe}` | IPC-01, IPC-02, IPC-04 | S0.7 |
| `forgedm-ipc::bootstrap` | NET-10, Inv-18 | S1.8 |
| `forgedm-core::{url_policy,naming,state}` | NET-05, NET-06, FS-01, SEC-01 | S1.1 |
| `forgedm-core::http::*` | NET-01, NET-02, NET-04, NET-06, NET-07 | S1.2 |
| `forgedm-core::{retry,rate_limit,progress,clock}` | NET-09 | S1.3 |
| `forgedm-core::{client,writer,finalize,hash,transfer::single}` | NET-03, NET-07, FS-02, FS-05, Inv-06 | S1.7 |
| `forgedm-core::{probe,checkpoint,resume}` | NET-02, DB-02 | S1.9 |
| `forgedm-core::transfer::{segment_table,worker,segmented}` | NET-01, PLAT-03 | S1.11 |
| `forgedm-core::engine::{hosts,admission,budgets}` | NET-08, NET-09, IPC-04 | S1.12, S1.13 |
| `forgedm-core::recovery` | DB-02, DB-03 | S1.14 |
| `forgedm-storage` | DB-01, DB-02, DB-04 | S0.6 |
| `forgedm-agent` | IPC-02 startup, SEC-01 logging, DB-04 sealer | S0.8, S1.7, S1.9, S1.14 |
| `forgedm-cli` | SEC-02, FS-03 (`--yes`) | S0.9, S1.7, S1.9 |
| `apps/desktop` | IPC-03, UI-01 | S0.10, S1.10 |
| `tests/fixtures` | SUP-02 | S0.5, S1.4 |
| Workspace config, scripts | SUP-01, SUP-02, PLAT-01 | S0.1, S0.11, S1.15 |

Security-trigger slices requiring both rust-reviewer and security-reviewer: S0.1, S0.3, S0.4, S0.6, S0.7, S0.8, S0.9, S1.1, S1.2, S1.5, S1.6, S1.7, S1.8, S1.9, S1.11, S1.12, S1.13, S1.14 (desktop slices use TS/react reviewers plus security).

## 8. Out of scope and accepted risks

1. **Same-user malicious software** at medium integrity can do anything ForgeDM can: connect to the pipe and issue legal actions, read the DB and unseal it with CryptUnprotectData, edit `.part` files, change proxy keys, replace binaries in a user-writable install path, swap directory components mid-run. The DACL, SID, image-path and integrity checks defend against other users, other sessions, accidental collisions and low-integrity or sandboxed processes, not a determined same-user attacker.
2. Elevated/SYSTEM malware, kernel compromise, malicious administrators.
3. Offline disk access beyond DPAPI (use BitLocker). An admin password reset makes sealed values unrecoverable (handled).
4. Compromised or enterprise TLS roots trusted by the OS store.
5. A byte-substituting server with matching length and validators (needs a checksum).
6. Plain-http downloads exposed to the network and any proxy.
7. Directory-chain TOCTOU (FS-02) under the single-user model.
8. Proxy-resolved destinations and DNS names resolving to private addresses (NET-06 Phase 1 residual).
9. Malicious content itself: ForgeDM neither scans nor sandboxes; it preserves MOTW and never executes downloads.
10. Resource use by legitimately large or slow files within configured limits.
11. Supply-chain compromise of a legitimate release before disclosure.
12. Path-embedded URL tokens in redacted displays (SEC-01).
13. **UNVERIFIED until tested:** header byte-size enforcement through reqwest/hyper; the effect of `reject_remote_clients` on the pipe (no readback API; IPC-01 has no test for it); low-IL client test method; symlink test privilege; IAE with `\\?\` paths; SmartScreen use of HostUrl; Attachment Manager policy on the direct-write fallback; real disk-full and removable-drive handling; cross-account pipe behaviour.
14. No telemetry, accounts or hosted backend exist, so those surfaces do not exist.

## 9. Gates tied to this model

- **GATE 1** (approved 2026-11-15; record in PHASE_0_1_PLAN section 1) ratified C4 (DACL without SYSTEM), C11 (per-value DPAPI), C17 (Content-Encoding rejection), C25 (literal-only private-destination policy in Phase 1), C28 (no approved-roots list in Phase 1), and decided D1 (prebuilt NASM objects), D3 (Node 26.13.1 LTS), D4 (llvm-cov and cargo-deny; no cargo-audit), D8 (sanitized MOTW URLs).
- Network, IPC and installer code are gated on this document's approval (execution prompt section 3), which GATE 1 gave on 2026-11-15.
- security-reviewer runs on every security-trigger slice; findings go to `docs/SECURITY_REVIEW.md`. Each phase gate requires zero open Critical/High in scope, otherwise the phase is PARTIAL or BLOCKED.
- Nothing in this document is evidence that a control is implemented or tested. Status is recorded only in `docs/IMPLEMENTATION_STATUS.md` and `docs/TEST_EVIDENCE.md`.
