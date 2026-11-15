# ECC Workflow for ForgeDM

Status as of 2026-11-15. This records what ECC actually provides on this machine and how ForgeDM uses it. It is based on inspecting the installed files, not on memory.

## 1. Installed ECC form

| Item | Finding | Evidence |
|---|---|---|
| Install type | Claude Code **plugin** `ecc@ecc` v2.2.3, user scope | `~/.claude/plugins/installed_plugins.json` |
| Plugin path | `~/.claude/plugins/cache/ecc/ecc/2.2.3` | directory listing |
| Agent naming | Plugin-namespaced: `ecc:<name>` (e.g. `ecc:planner`). No manual copy in `~/.claude/agents/` | `~/.claude/agents` does not exist |
| Rules installed | `~/.claude/rules/ecc/{common,python,web}` (global) | directory listing |
| Rules **not** installed | `rust`, `typescript`, `react` packs exist in the plugin (`rules/rust`, `rules/typescript`) but are not active | plugin `rules/` listing |
| Skills | 314 skill folders in the plugin, including `orch-build-mvp`, `orch-pipeline`, `tdd-workflow`, `verification-loop`, `rust-patterns`, `rust-testing`, `security-review`, `ecc-guide` | plugin `skills/` |
| Hooks | Plugin `hooks/hooks.json` (PreToolUse, PostToolUse, Stop, SessionStart/End, PreCompact) | plugin `hooks/README.md` |

Nothing was reinstalled, copied, or reconfigured. No global settings, MCP servers, or permissions were changed.

## 2. Hooks that affect this project

| Hook | Effect on ForgeDM work |
|---|---|
| GateGuard fact-forcing (`pre:bash:gateguard-fact-force`, edit/write variant) | Blocks the first Bash command, commands it considers destructive, and the first write to a new file until the request, the target files, and a rollback step are stated. We comply rather than disable it. |
| Pre-commit quality check | Runs on `git commit`: lints staged files, validates the conventional commit message, and looks for secrets and debug statements. |
| Dev-server blocker | Blocks `npm run dev`-style commands outside tmux. Relevant for `pnpm tauri dev` in Phase 2: use a background or preview launch instead. |
| Post-edit quality gate / TypeScript check / Prettier | Runs after edits; TS checks apply to `apps/desktop`. |
| Doc file warning | Warns about `.md` files outside allowed paths. Planning docs live in `docs/` to avoid this. |
| Stop hooks (console.log audit, session summary, cost tracker) | Informational. Session summaries are not relied on: repo docs are the source of truth. |

## 3. Chosen workflow: `orch-build-mvp` on the `orch-pipeline` engine

- **Size tier: large.** There is a spec document, new external dependencies, a public IPC contract, and security triggers (network input, file paths, IPC, DB, secrets).
- **Phase mask:** 0 Intake → 1 Research & Reuse → 2 Plan → **GATE 1** → 3 Scaffold → 4 Implement (TDD) → 5 Review → 6 Commit (**GATE 2**).
- **GATE 1 (plan approval):** no implementation code before the user approves `docs/PHASE_0_1_PLAN.md`.
- **GATE 2 (pre-commit):** the diff summary and proposed conventional commit messages are shown and confirmed before every commit.

### Agent and skill map used

| Pipeline phase | ECC component (verified installed) | Notes |
|---|---|---|
| Research | general-purpose research agents (crates.io/npm registries, docs.rs, official docs) | `ecc:docs-lookup` needs Context7 MCP, which is **not connected**, so this is the stated local equivalent |
| Plan | `ecc:planner`, `ecc:architect`, `ecc:security-reviewer` (threat model) + an independent completeness critic | Run in parallel, then reconciled |
| Implement | `tdd-workflow` skill discipline (red → green → refactor), `ecc:tdd-guide` | Parallel implementers only on disjoint file ownership in the main checkout (section 5); otherwise serialized |
| Build failures | `ecc:rust-build-resolver`, `ecc:build-error-resolver` | |
| Review | `ecc:rust-reviewer`, `ecc:security-reviewer` (all security-trigger slices), `ecc:code-reviewer`, `ecc:silent-failure-hunter`, `ecc:pr-test-analyzer`; `ecc:typescript-reviewer` / `ecc:react-reviewer` for the desktop shell | Fresh-context reviewers; findings are verified before they're acted on |
| Verification | `verification-loop` skill | fmt / clippy / test / build, with outputs recorded in `docs/TEST_EVIDENCE.md` |

### Deviation: `/gan-build` inner loop not used for Phase 0/1

`orch-build-mvp` suggests driving slices with `/gan-build --skip-planner`. For Phase 0/1 we use the pipeline's TDD + review phases instead, because:
1. The `gan-generator` step commits on every iteration, which conflicts with GATE 2 and with the rule to commit only on user confirmation.
2. Its default evaluator is Playwright UI testing. Phase 0/1 slices are engine/agent/CLI work, where `cargo test` against adversarial fixtures is the real evaluator.

We'll revisit for Phase 2 (premium UI) with `--eval-mode screenshot|playwright`, with its commit step gated.

## 4. Capabilities absent or needing approval

| Capability | Status | Consequence |
|---|---|---|
| Context7 MCP (`ecc:docs-lookup`) | Not connected | Docs checked via registries and official sites instead |
| AgentShield | Not installed | Not run. It covers agent/hook/MCP config surfaces, not the Rust app. Installing it needs your approval |
| `cargo-llvm-cov` (coverage) | **Installed** 0.9.1 (+ `llvm-tools-preview`), 2026-11-15 | Coverage gate measurable |
| `cargo-deny` (advisories, licences, bans) | **Installed** 0.20.2, 2026-11-15 | Run as `cargo deny --locked check ...` |
| `cargo-audit` | Not installed (coordinator decision) | Its advisory check is redundant with cargo-deny's |
| ECC `rust` / `typescript` rule packs | Present in the plugin; **not installed (decided 2026-11-15)** | We ran the supported installer as a dry run: `node scripts/install-apply.js --target claude-project --dry-run rust`. It planned 936 operations, including every language's rules, a second copy of the common rules, `agents-core`, `commands-core` and `hooks-runtime`. The only rules-only module, `rules-core`, also copies every language. Installing either would duplicate the ECC install, which the execution prompt forbids. Rust guidance instead comes from `ecc:rust-reviewer` and the `rust-patterns` / `rust-testing` skills. |

GATE 1 outcomes (user, 2026-11-15): plan approved as written. Node 26.13.1 LTS approved. Optional tooling was left to the coordinator: `llvm-tools-preview`, `cargo-llvm-cov` and `cargo-deny` are installed; `cargo-audit` is not installed because cargo-deny's advisory check covers it. AgentShield is still not installed.

## 5. Orchestration conventions

- Fan-out runs through Claude Code workflows. Each parallel implementer owns a disjoint set of paths listed in the plan; overlapping work is serialized.
- **Deviation from plan section 0, rule 1 (worktrees):** parallel slices work in the main checkout. Each implementer is limited to its pre-declared paths, the lockfile is frozen (`--locked`), and each gets its own `CARGO_TARGET_DIR` under `target/`. Reason: GATE 2 requires user confirmation before every commit, and merging worktree branches would need commits nobody had confirmed. Formatting is crate-scoped (`cargo fmt -p <crate>`), never `--all`, while other slices are in flight.
- Reviewers never review their own code. Every Critical/High finding is either fixed with a regression test or recorded as a blocker in `docs/IMPLEMENTATION_STATUS.md`.
- Session continuity: `docs/NEXT_STEPS.md` is the source of truth for resuming work. ECC session-summary hooks are a convenience, not a dependency.
