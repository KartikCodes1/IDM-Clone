# ForgeDM

Personal Windows 11 download manager. A Rust agent owns the queue, DB and disk; the CLI, desktop (Tauri 2) and future native host are pipe clients.

Source of truth:
- `docs/NEXT_STEPS.md` for where work stands
- `docs/PHASE_0_1_PLAN.md` for slices; Appendix A is the canonical wire contract
- `docs/ARCHITECTURE.md` and `docs/THREAT_MODEL.md` for design and security invariants

## Commands (repo root; the path has a space, so quote it)
- Git Bash: `export PATH="$HOME/.cargo/bin:$PATH"`. The toolchain is pinned to 1.100.0 by `rust-toolchain.toml`.
- `cargo build --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo fmt --all --check`. While other slices are in flight, format only your crate with `cargo fmt -p <crate>`.
- Node for `apps/desktop`: `%LOCALAPPDATA%\nvm\v26.13.1`. Don't run `nvm use`; it changes the user's global Node. pnpm is 10.29.2.

## Invariants (release blockers; see THREAT_MODEL section 5)
- Every cargo command uses `--locked`. Only the coordinator changes `Cargo.toml` dependencies or `Cargo.lock`.
- `unsafe` lives only in `crates/forgedm-platform`, and every block has a `// SAFETY:` comment.
- reqwest: no decompression features, rustls only, `http1_only`, no total timeout, redirects followed manually. `clippy.toml` bans TLS bypasses, `std::fs::rename` and `reqwest::Client::new`.
- Never write bytes before the response passes the range decision table. Flush, then commit to the checkpoint. Never overwrite an existing file. MOTW goes on every completed file.
- Secrets: URLs are sealed at rest (DPAPI). Logs, UI and errors only ever show redacted URLs.
- The IPC pipe uses an explicit DACL plus `first_pipe_instance`. Clients verify the server's SID and full exe path before sending anything.

## Gates
- Tests come first (RED, then GREEN). Fresh-context ECC reviewers check every slice. Zero open Critical/High before a slice is done.
- Evidence goes in `docs/TEST_EVIDENCE.md`: real commands and exit codes. Anything not run is recorded as NOT RUN.
- Never commit without the user's GATE 2 confirmation. Never push without asking. Use conventional commits.
- Don't use `tauri dev`, which the ECC hook blocks. Use `tauri build --debug --no-bundle`.
