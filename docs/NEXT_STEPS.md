# Next Steps (source of truth for resuming)

Updated 2026-11-15.

## Where we are
- GATE 1 was approved on 2026-11-15, accepting all recommendations. The record is in PLAN section 1 and ECC_WORKFLOW section 4.
- **S0.1 workspace scaffold: Tested. S0.2 docs: Implemented.** Both are reviewed, with zero open Critical/High. Nothing is committed yet.
- PCR-01 and PCR-02 are decided and applied to Appendix A (PLAN 11.1).
- Tools:
  - Rust 1.100.0, pinned; the `stable` toolchain is also present
  - cargo-llvm-cov 0.9.1, cargo-deny 0.20.2
  - Node 26.13.1 at `%LOCALAPPDATA%\nvm\v26.13.1` (the user's global Node stays 25.5.0)
- `CLAUDE.md` exists and stays concise.

## Next
1. **Done:** GATE 2 confirmed, commits `a47b9d5` (scaffold) and `b1e8727` (docs + specs). The user chose **one commit confirmation per wave**, with one commit per slice.
   - Reusable wave workflow script: `~/.claude/projects/C--Users-karti-Desktop-IDM-Clone/<session>/workflows/scripts/forgedm-wave-*.js`
   - It takes args `{wave, slices:[{id,title,crate,owns,refs,reviewers,notes}]}`.
2. **W1 done and committed** (`02c4656`..`6af19d5`): S0.3, S0.4 and S0.5 tested; 67 workspace tests pass. PCR-03 and PCR-04 decided.
3. **W2 done and committed** (`93c8bc9`..`d942573`): S0.4b, S0.6, S0.7, S1.1 and S1.2 are tested; 218 workspace tests pass. PCR-05 is decided.
4. **W3 is done** (gate green: 368 cargo tests and 14 vitest tests pass; desktop debug build OK; coordinator re-run matched). It contained:
   - S0.8 agent
   - S1.4 adversarial fixture (6.9 GB runaway fixed: bodies streamed, log and multipart capped, memory regression test)
   - S1.3 retry/rate/ETA
   - S0.10 Tauri desktop shell (adds 251 lock packages; 0 existing versions changed)

   The coordinator restored the agent's `fault-injection` feature (S0.8 had removed it).
   - Node 26 for pnpm *and* Tauri's child `pnpm run build`: put `scratchpad/node26shim` (pnpm.cmd wrapper running v26 node with `v25.5.0/node_modules/pnpm/bin/pnpm.cjs`) first on PATH, followed by `%LOCALAPPDATA%\nvm\v26.13.1`. If the scratchpad was cleared, recreate the shim.
   - A memory watchdog (scratchpad `mem-watchdog.log`) kills ForgeDM target binaries over 2.5 GB until about 06:40.
   - **W3 follow-ups:**
     - S1.8 exports `ipc::default_pipe_name()`.
     - A small desktop follow-up (S0.10b, with S1.10) does three things: it uses that function instead of `whoami.exe`; it installs a tracing subscriber writing to `%LOCALAPPDATA%\ForgeDM\logs\desktop.*` (coordinator approves the `tracing-subscriber` + `tracing-appender` edges; this resolves the open S0.10 Medium); and it logs startup errors.
     - S1.15 removes unused deps (anyhow, serde in agent; others found by `unused_dependencies`).
     - Agent `--data-dir` user-only ACL check and log-filter scoping (S0.8 security Lows) are deferred to S1.14 / Phase 2.
5. **Then:** W4 (S0.9 CLI, S1.5 fs, S1.6 MOTW, S1.8 bootstrap) and S0.11, the Phase 0 gate.
4. **W1 slices as originally planned (all done):**
   - S0.3 Protocol v1 (owns `crates/forgedm-protocol/**`)
   - S0.4 Platform security primitives (platform files per PLAN S0.4)
   - S0.5 Fixture baseline (`tests/fixtures/**`)

   Each slice gets its own `CARGO_TARGET_DIR=target/<slice>`.
3. **W2:** S0.6 storage, S0.7 IPC, S1.1 url/naming/state, S1.2 range/redirect (merge after S1.1).
4. Continue the waves according to PLAN section 6.

Every slice: RED tests -> GREEN -> fmt/clippy/test -> reviewers -> verify findings -> fix Critical/High (and Medium) -> evidence -> GATE 2.
