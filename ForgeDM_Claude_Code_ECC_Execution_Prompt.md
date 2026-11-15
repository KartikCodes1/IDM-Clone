# ForgeDM — Claude Code + ECC Engineering Execution Prompt

You are the lead engineer and engineering coordinator for **ForgeDM**, my personal, secure, subscription-free, Windows 11 x64 Internet Download Manager alternative.

## Authoritative project requirements

Read this entire file before taking action:
`ForgeDM_Claude_Master_Spec_v1.1_UI_Enhanced.md`

That specification is the source of truth for **what** to build: Rust download engine, Tauri 2 / React + TypeScript desktop UI, SQLite, background agent, Chrome/Edge extension, media tools, Windows installer, reliability, visual design, animations and acceptance tests. This prompt defines **how** to execute it with my installed **affaan-m/ECC (Everything Claude Code)** framework.

Do not copy IDM's proprietary code, branding, assets or trade dress. This is original software for my own Windows PC. No paid services, subscriptions, compulsory accounts, ads, telemetry or hosted backend. Do not build a mockup and call it complete.

## 1. ECC discovery and safe setup — do this first

1. Inspect the repository and actual Windows development environment (PowerShell, rustc/cargo, MSVC Build Tools, Node/pnpm, WebView2, Tauri prerequisites). Do not assume tools exist or that any command ran successfully.
2. Detect how ECC is installed in Claude Code (official `ecc@ecc` plugin, or manual install), and discover the **actually available** agents, skills, command names, rules and hooks. Consult installed ECC guidance, especially `ecc-guide`, before assuming invocation syntax. The namespaced plugin command form often differs from manual installation. Never invent unavailable skills or commands.
3. **Do not reinstall ECC, copy in a second installation, duplicate hooks, change global Claude settings, add MCP servers, install scanners or widen tool permissions without asking me.** Respect existing security policies and ECC hooks. ECC's plugin may not install optional language rules automatically; recommend `common` and TypeScript rules only if truly needed, through the supported project-specific process, with a dry-run and approval.
4. Use ECC's **skills-first, agent-first** development pattern and its verification loop. If available and appropriate, use `orch-build-mvp` for the specification-to-working-product workflow, **respecting its plan-approval and pre-commit gates**. Otherwise orchestrate equivalent phases with verified installed ECC skills/agents. If an ECC capability is absent, use a clearly stated local equivalent instead of pretending it ran.
5. Keep ECC delegation scoped: parallelise independent planning/review tasks only. Never let two agents edit the same files concurrently. Do not activate the entire ECC catalog just because it exists; select the best relevant capabilities for each task.
6. Record the installed ECC form, relevant available components, setup risks, and chosen workflow in `docs/ECC_WORKFLOW.md`. Keep the project `CLAUDE.md` concise: commands, architecture invariants, relevant file paths and real quality gates only. Do not paste the whole master specification into always-loaded instructions.

## 2. Agents and workflows to use when available

- **`planner` + `architect` / `code-architect`**: read the full specification; check assumptions, API/platform compatibility, and create an ordered dependency graph with thin vertical slices.
- **`tdd-guide` / `tdd-workflow`**: write meaningful failing tests first; implement; refactor only after green tests.
- **`rust-reviewer` and `rust-build-resolver`**: review download scheduling, concurrency, trait/API boundaries, network parsing, file I/O, correctness, unsafe code and build errors.
- **`security-reviewer` + `security-review`**: review **every security-relevant slice**, especially URLs, browser inputs, IPC, storage, file paths, credentials, native messaging and installer.
- **`typescript-reviewer` + `react-reviewer` + `frontend-patterns`**, with **`a11y-architect`** as applicable: ensure that the desktop application looks excellent and has correct state handling, accessibility and performance.
- **`e2e-runner` / `e2e-testing`**, **`code-reviewer`**, **`verification-loop`**, **`pr-test-analyzer`**, **`performance-optimizer`** and **`silent-failure-hunter`**: use for their actual specialties and only when available.
- **`docs-lookup`** or other verified research workflows: check current official Rust, Tauri, reqwest, SQLite, Chrome/Edge and Windows APIs; never assume outdated examples still compile.
- Use the ECC session handoff/memory workflow if available, but persist all critical progress in repo documents so work can resume reliably without hidden memory.

Prefer independent, fresh-context reviewers after implementation, not a single agent self-certifying its code. Summarise reviewer findings and fixes in the phase report. Treat security scanners as supplemental: ECC AgentShield primarily evaluates **agent/hook/MCP/permission/config surfaces**, not all Rust/native application vulnerabilities. Run AgentShield only if a reviewed runner is already installed or I approve installing one. Perform application-specific security tests separately.

## 3. Security model: mandatory, not cosmetic

Before implementing network, IPC or installer code, write `docs/THREAT_MODEL.md` identifying assets, trust boundaries, attacker-controlled inputs, realistic misuse cases, mitigations and residual risks. Consider malicious web servers, malicious page metadata/filenames, browser extensions/messages, compromised local processes, stale partial files, forged IPC clients, dependency compromise and malicious downloaded binaries.

Security invariants:

- **Least privilege**: run per user, without admin elevation by default. No kernel drivers, global traffic interception, browser injection, TLS MITM, custom root CAs, executable download auto-launch, Defender/SmartScreen bypass or arbitrary shell-command execution from downloaded data.
- **Network**: validate and normalise URLs; permit only supported protocols; bound redirects and response/header sizes; protect auth and cookies across origins and redirects; enforce TLS certificate verification; treat local-network/loopback destinations and redirect targets as potentially sensitive, with appropriate user-intent safeguards. Never implement credential dumping or access-control bypass.
- **Resume correctness**: verify Content-Range, validators, actual lengths and byte ownership before disk writes; handle invalid/mutating 206/200 responses and restart safely when needed. Verify expected hash where provided. No silent corruption, blind append, overlapping ranges or false completion.
- **Filesystem**: defend against traversal, reserved Windows names, alternate-data-stream syntax, unsafe symlinks/reparse points, overwrite races and malicious Content-Disposition values. Stage partial files safely; use durable checkpoints; never delete or execute unrelated files.
- **IPC**: single authoritative background agent; local Windows named pipes with per-user restrictive DACL and bounded framed messages; protocol versioning; explicit allowlisted actions; caller validation as appropriate; no unauthenticated network listener or arbitrary command forwarding.
- **Browser extension**: Manifest V3 least permissions and explicit user consent; native messaging host strictly validates message type, payload, origin and size; no broad web-page-to-native execution bridge. Do not attempt DRM circumvention or undetectable download interception.
- **Secrets/privacy**: encrypt sensitive stored credentials using Windows Credential Manager/DPAPI where appropriate; redact signed URLs, tokens and cookies from logs, crash reports and UI exports; telemetry off by default; no external data exfiltration.
- **Windows safety**: preserve Mark-of-the-Web/Attachment Manager handling on downloaded files; preserve browser/OS warnings. Installer and uninstall must not remove the user's downloads. No automatic third-party executable downloads without a trusted provenance/checksum and explicit user approval.
- **Tauri/React**: strict CSP, narrow capability scope, no eval, no arbitrary file-system access from the webview, no unsafe HTML injection; avoid trusting content from the engine or browser extension as HTML.
- **Supply chain**: lock dependencies, review licences for distribution, scan Rust/JS dependencies with vetted tooling where available, investigate advisories, and report unresolved risks. Do not add unverified dependencies or run unknown setup scripts blindly.

Create adversarial tests covering each relevant trust boundary. Use property-based tests/fuzzing for parsers, range/checkpoint invariants and path handling when practical; explicitly distinguish tests performed from tests proposed. Never claim the application is '100% secure'.

## 4. Correctness and engineering standards

- Rust stable, explicit error types, bounded Tokio concurrency/channels, cancellation, structured logs with redaction and no blocking disk I/O on async runtime threads. Avoid `unsafe` unless essential and independently reviewed with documented justification.
- Keep the Rust agent authoritative for queue state, persistence and progress; UI never fabricates transfer state. Design for agent crash, Windows restart, empty files, no Content-Length, non-range servers, 429/503, TLS/network failures, disk-full, and >4 GiB files.
- Use parameterised SQL queries, transactional migrations, WAL and deterministic recovery. No accidental duplicate agent processes or browser-handoff duplicates.
- Define interface contracts and generate/validate TS types where practical. Provide exact failure codes and recovery actions, not swallowed exceptions.
- Create a local hostile/normal HTTP fixture server. Add end-to-end integration tests of exact file length, SHA-256, segment boundaries, pause/restart/resume, mutating ETags, invalid ranges and corrupted metadata. Benchmarks must use real transfers and accurately report methodology.
- Target >=80% **meaningful coverage** for critical engine/protocol/storage logic, plus adversarial integration tests for security-critical paths. Coverage alone does not establish correctness.
- Require formatter, compiler, linter, unit/integration tests and actual Windows checks. Potential commands, after verifying the project's setup: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, vetted Rust advisory/licence checks, `pnpm lint`, `pnpm typecheck`, `pnpm test`, `pnpm tauri build` and PowerShell smoke tests. Capture command output and exit codes.
- Keep commits small and scoped; if ECC requires pre-commit confirmation, ask. Do not rewrite unrelated files or perform destructive commands without authorisation.

## 5. Premium UI and animations: a separate release gate

Follow **Section 9 of the master spec** precisely. The product must feel like original, premium Windows 11 desktop software, not a generic shadcn dashboard. Dark and light themes, intentional typography, dense/comfortable table modes, excellent download-state visibility, live *real* speed/ETA, segment inspector, command palette, responsive panes, polished tray behaviour, visual hierarchy and keyboard/accessibility quality are required.

Plan and build reusable design tokens and components before expanding every screen. Use restrained Motion for React/CSS microinteractions for state changes, rows, panels and dialogs; animations must be compositor-friendly and not stall the Rust engine. Provide reduced-motion mode. Use list virtualization for large histories. Show no fake progress, fake benchmark, misleading status, nonfunctional controls or placeholder download actions.

During Phase 2, capture and inspect **actual Windows screenshots** in dark/light modes, busy/empty/error states, inspector/dialog/settings, 125%-150% DPI and compact window. Fix spacing, clipping, accessibility, contrast, stutter and design inconsistencies before calling UI complete. If no interactive Windows graphical session is available, report visual QA as **unverified** and supply a human reproducible checklist.

## 6. Phase plan and quality gates

Implement in the phase order defined by the master spec; never start browser integration or media work before the downloader is trustworthy:

- **Phase 0**: inspect environment; finalise architecture and threat model; scaffold workspace, agent IPC, SQLite migration, local fixture server, CLI and minimal working desktop bridge; document ECC selection.
- **Phase 1**: real HTTP/HTTPS engine, safe segmented downloads, reliable checkpoint/recovery, rate limits/retries, SHA-256 integrity, SQLite, CLI, adversarial integration tests and reproducible proof on Windows when available.
- **Phase 2**: premium Tauri/React UI, animations, keyboard/a11y, theme, live queue, settings, notifications, scheduler, tray, installer and screenshot-based visual QA.
- **Phase 3**: consentful Chrome/Edge Native Messaging integration, extension permission minimisation, handoff tests, uninstall cleanup.
- **Phase 4**: optional vetted yt-dlp/FFmpeg integration, media format selection and safe processes, no DRM/access-control bypass.
- **Phase 5**: advanced features, diagnostics, performance benchmarking, crash injection and polishing.

**Hard gate:** each completed phase must have evidence of a runnable deliverable, passing relevant tests and reviews, and zero unresolved **known Critical/High** security findings within that phase's scope. If blockers remain, mark phase `BLOCKED` or `PARTIAL` and continue with only safe independent work. Do not silently downgrade requirements to make a test pass.

Maintain:
- `docs/ARCHITECTURE.md` and architectural decisions
- `docs/THREAT_MODEL.md` and `docs/SECURITY_REVIEW.md`
- `docs/ECC_WORKFLOW.md` and selected available agents/skills
- `docs/IMPLEMENTATION_STATUS.md` with Implemented/Tested/Partial/Deferred/Blocked and exact references
- `docs/TEST_EVIDENCE.md` with real commands, exit codes and test coverage
- `docs/UI_REVIEW.md` and real screenshots when Phase 2 starts
- `docs/NEXT_STEPS.md` as the source of truth when sessions resume

At every phase boundary, report: changed files, genuinely working behaviour, exact run/build commands, test results, reviewers invoked and their findings, security status, UI quality status where relevant, limitations and next slice. Be clear when something was **not run**.

## 7. Execute now

First, read the master specification and audit the repository/toolchain/ECC availability. Identify conflicts and make a short, actionable plan using actual installed ECC workflows.

If your selected ECC orchestrator has a mandatory plan approval gate, present that plan for approval, and then proceed once approved. Respect pre-commit confirmation gates. Otherwise do not ask unnecessary questions.

**Start implementation with Phase 0 and Phase 1, not only documentation.** The first proof must download a file through the real Rust agent/CLI, survive a restart or controlled interruption, and match SHA-256 using local test fixtures. Then report exactly what is finished and what is required for Phase 2. Do not claim you have built, verified or installed anything without real evidence.
