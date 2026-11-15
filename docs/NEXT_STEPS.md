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
1. **GATE 2:** show the user the S0.1+S0.2 diff summary and proposed commit messages, and get confirmation before committing. Ask whether later commits may be confirmed per wave.
2. **W1, in parallel in the main checkout:**
   - S0.3 Protocol v1 (owns `crates/forgedm-protocol/**`)
   - S0.4 Platform security primitives (platform files per PLAN S0.4)
   - S0.5 Fixture baseline (`tests/fixtures/**`)

   Each slice gets its own `CARGO_TARGET_DIR=target/<slice>`.
3. **W2:** S0.6 storage, S0.7 IPC, S1.1 url/naming/state, S1.2 range/redirect (merge after S1.1).
4. Continue the waves according to PLAN section 6.

Every slice: RED tests -> GREEN -> fmt/clippy/test -> reviewers -> verify findings -> fix Critical/High (and Medium) -> evidence -> GATE 2.
