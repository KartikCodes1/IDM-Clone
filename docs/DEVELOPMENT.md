Status: ACCEPTED at GATE 1 (2026-11-15). Implementation is in progress; docs/IMPLEMENTATION_STATUS.md records what is actually built and tested.

# ForgeDM Development Guide

**Last updated:** 2026-11-15
**Scope:** how to build, test and check the Phase 0/1 code. Commands are the ones defined in `docs/PHASE_0_1_PLAN.md` (sections 0, 8). A command listed here is not evidence that it passes. Real results go in `docs/TEST_EVIDENCE.md`.

Repo root = Cargo workspace root: `C:\Users\karti\Desktop\IDM Clone`. The path contains a space: always quote it.

---

## 1. Prerequisites

Platform: Windows 11, x64. A non-admin user is enough.

| Tool | Version | Notes |
|---|---|---|
| rustup, Rust stable | rustc 1.100.0 (default) | Precondition: `rustc --version` works. |
| Rust pinned toolchain | `1.100.0` with rustfmt, clippy, llvm-tools-preview | `rust-toolchain.toml` pins it (D2). It is a separate rustup toolchain from stable. Installed once with `rustup toolchain install 1.100.0 --profile minimal -c rustfmt -c clippy -c llvm-tools-preview`. |
| Workspace MSRV | `rust-version = "1.94"` | sqlx 0.9 minimum. |
| MSVC Build Tools 2022 | linker | Verified with a scratch build. |
| NASM | not required | D1: `AWS_LC_SYS_PREBUILT_NASM = "1"` in `.cargo/config.toml`. |
| cargo-llvm-cov, cargo-deny | installed (D4) | `cargo install --locked cargo-llvm-cov cargo-deny`. |
| cargo-audit | not installed, not used | cargo-deny advisories replace it. |
| Node.js | 26.13.1 LTS (D3) | `engines.node = "^24.15.0 \|\| ^26.0.0"`. |
| pnpm | 10.29.2 (D5) | Pinned by `packageManager`; avoids auto-download. |
| WebView2 runtime | any installed version | Detected by `scripts/check-env.ps1` (added in S0.11). |

Bash sessions must first run:

```
export PATH="$HOME/.cargo/bin:$PATH"
```

---

## 2. Setup

```
powershell -NoProfile -File scripts\check-env.ps1     # added in S0.11; read-only detection
pnpm install --frozen-lockfile                        # committed lockfile only
```

Before `Cargo.lock` exists, `cargo fetch` is the first step (D5). After it exists, every cargo command uses `--locked`.

---

## 3. Build and test commands

Source: PLAN section 8 (and the S0.1 and S1.9 evidence lines). Phase gate sets: section 8.1 (end of Phase 0), section 8.2 (end of Phase 1).

### 3.1 Rust

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --bins --locked
cargo build --workspace --locked
cargo metadata --locked --no-deps
cargo test --workspace --locked
cargo test -p forgedm-core --test tls_smoke --locked
cargo test -p forgedm-agent --features fault-injection --test crash_matrix --locked
cargo test --workspace --locked -- --include-ignored
cargo test -p forgedm-cli --test e2e_first_proof --locked
cargo llvm-cov -p forgedm-core -p forgedm-protocol -p forgedm-storage --locked --fail-under-lines 80
cargo deny --locked check advisories licenses bans sources
```

- `cargo fmt --all --check` has no `--locked` flag (it does not resolve dependencies), so it is written as the plan defines it.
- `cargo deny` takes `--locked` as a top-level option, placed before the subcommand (`cargo deny --locked check ...`). Verified with `cargo deny --help` (cargo-deny 0.20.2 lists `--locked`, `--frozen`, `--offline`). D5 applies to it.
- `cargo fmt --all --check` is check-only. Never run `cargo fmt --all` (it rewrites files) while other slices are in flight. Format one crate with `cargo fmt -p <crate>`.
- `--include-ignored` is the slow tier (5 GiB download, IAE). Phase 1 only.
- `cargo llvm-cov` and `cargo deny` are D4 tools. If they are not run, record NOT RUN.

### 3.2 Desktop (pnpm)

```
pnpm install --frozen-lockfile
pnpm -r lint
pnpm -r typecheck
pnpm -r test
pnpm audit --prod
pnpm --filter @forgedm/desktop tauri build --debug --no-bundle
```

Do not use `tauri dev` (section 5).

### 3.3 Scripts

| Script | Status | Purpose |
|---|---|---|
| `scripts\check-env.ps1` | added in S0.11 | Read-only environment detection |
| `scripts\check.ps1` | added in S0.11 | Named static checks, PLAN 8.3 |
| `scripts\windows-smoke.ps1` | added in S0.11 | Build bins, start agent on a temp data dir, status/list/stop |
| `scripts\phase1-proof.ps1` | Phase 1 (S1.9, gate S1.15) | First resume proof |

Run any script as: `powershell -NoProfile -File scripts\<name>.ps1`. If execution policy blocks it, use `-ExecutionPolicy Bypass` for that one run only (PLAN 8.2).

Named checks in `check.ps1` (each prints PASS, FAIL or NOT RUN; full method in PLAN 8.3): `no-tls-bypass`, `no-assert-sql-safe`, `unsafe-only-in-platform`, `command-new-allowlist`, `layering`, `no-fixture-in-shipped`, `no-fault-injection-in-release`, `no-reqwest-compression`, `tauri-versions-match`, `no-loose-versions`, `lockfiles-committed`, `manifest-asinvoker`, `eslint-bans`. (`agent-has-no-tcp-listener` runs inside `windows-smoke.ps1`.)

---

## 4. Per-slice CARGO_TARGET_DIR

Parallel slices run in the main checkout, not in git worktrees (`docs/ECC_WORKFLOW.md` section 5). Each in-flight slice builds into its own target directory so builds do not block each other.

PowerShell:

```
$env:CARGO_TARGET_DIR = "C:\Users\karti\Desktop\IDM Clone\target\<slice-id>"
```

Bash:

```
export CARGO_TARGET_DIR="C:/Users/karti/Desktop/IDM Clone/target/<slice-id>"
```

Rules:

- `<slice-id>` is the slice ID, for example `S0.3`. `target/` is git-ignored.
- Never share one target directory between two in-flight slices.
- Each slice edits only the paths in its slice entry's Owns list (PLAN sections 4-5), including the modules pre-declared by S0.1.
- If a build script fails on the spaced path, use a `CARGO_TARGET_DIR` without spaces and record it (PLAN S0.1 fallback).
- Root `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml` and `clippy.toml` belong to the coordinator. A slice may add `[[bin]]` or features to its own crate manifest, never dependencies. A slice that needs a new dependency stops and files a request (PLAN section 0, rule 2).

---

## 5. ECC hooks that matter

Hooks are configured by the ECC plugin (`docs/ECC_WORKFLOW.md` section 2). Do not disable them.

- **GateGuard (fact-forcing).** It blocks the first Bash command, commands it considers destructive, and the first write to a new file. Comply: state briefly which files you touch, the rollback step, and the user's instruction, then retry the identical call.
- **Dev-server blocker.** It blocks `npm run dev`-style commands outside tmux. Never run `pnpm tauri dev`. Phase 0/1 verifies the desktop shell with `pnpm --filter @forgedm/desktop tauri build --debug --no-bundle`. Reason: Tauri's CSP is not applied under `tauri dev` with a Vite devUrl (ARCHITECTURE.md ADR-022).
- **Pre-commit quality check.** It runs on `git commit`: lints staged files, validates the conventional commit message, and looks for secrets and debug statements. Commits happen only at GATE 2, after the user confirms the diff summary and proposed messages. Types: feat, fix, refactor, docs, test, chore, perf, ci.

---

## 6. Evidence rules

- Record only commands that were actually run, with the real exit code, test counts and date, in `docs/TEST_EVIDENCE.md`.
- A check that was not run is recorded as **NOT RUN**, with the reason. A proposed test is never recorded as passing.
- Write the failing test first and record its RED output before the implementation.
- If a test fails, fix the implementation. Change a test only if the test is wrong, and say so. Never weaken a test or a lint to get green.
- Do not mark anything Implemented or Tested in `docs/IMPLEMENTATION_STATUS.md` unless the code exists and the tests ran.
- A slice is done only when it is Tested, its reviews are clean, and it has zero open Critical or High findings.
- UNVERIFIED items in `docs/THREAT_MODEL.md` must be proven by a test before they are relied on. Otherwise record NOT RUN.

---

## 7. Related documents

- `docs/PHASE_0_1_PLAN.md`: slices, GATE 1 record, and Appendix A (canonical protocol, states, error codes, exit codes, constants, names). Appendix A changes only through a protocol-change request.
- `docs/ARCHITECTURE.md`, `docs/THREAT_MODEL.md`: design and threat controls.
- `docs/ECC_WORKFLOW.md`: how ECC is installed and used.
- `docs/IMPLEMENTATION_STATUS.md`, `docs/TEST_EVIDENCE.md`, `docs/SECURITY_REVIEW.md`, `docs/NEXT_STEPS.md`: tracking.
