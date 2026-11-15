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
3. **W2 is running** with dependency ordering (the wave script's `after` field):
   - S0.4b (pipe returns tokio server) runs alongside S0.6 storage.
   - S0.7 IPC and S1.1 url/naming/state start after S0.4b.
   - S1.2 range/redirect starts after S1.1.

   `.gitattributes` (`* text=auto eol=lf`) is staged for the W2 commit batch.
4. **W1 slices as originally planned (all done):**
   - S0.3 Protocol v1 (owns `crates/forgedm-protocol/**`)
   - S0.4 Platform security primitives (platform files per PLAN S0.4)
   - S0.5 Fixture baseline (`tests/fixtures/**`)

   Each slice gets its own `CARGO_TARGET_DIR=target/<slice>`.
3. **W2:** S0.6 storage, S0.7 IPC, S1.1 url/naming/state, S1.2 range/redirect (merge after S1.1).
4. Continue the waves according to PLAN section 6.

Every slice: RED tests -> GREEN -> fmt/clippy/test -> reviewers -> verify findings -> fix Critical/High (and Medium) -> evidence -> GATE 2.
