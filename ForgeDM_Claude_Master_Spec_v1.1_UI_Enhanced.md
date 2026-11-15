# ForgeDM — Claude Code Master Engineering Specification
Version: 1.1 (premium UI and motion requirements) | Target: Windows 11 x64 | Product: personal, offline-first, subscription-free download manager

## Your role and objective
Act as a principal Rust systems engineer, senior Windows desktop developer, React/TypeScript engineer, security reviewer, and QA lead. Build ForgeDM as a real, installable Windows 11 download manager inspired by the *capabilities* of Internet Download Manager (IDM), not its branding, proprietary code, or trade dress. Do not build a mock-up. The download engine, persistence, UI controls, installer, browser extension, and tests must work together. Use original branding and assets.

I own the software and want **no mandatory subscription, paid services, account, hosted backend, ads, or telemetry**. All primary functionality must run locally. Open-source libraries are allowed. Optional external binaries (yt-dlp, FFmpeg, Deno) must be installed/configured transparently and remain replaceable. This is an ambitious project: implement it in verified, runnable vertical slices; never claim that every feature is finished simply because the UI shows it.

## Execution contract for Claude
1. Inspect the existing repository and toolchain first. Report assumptions and show the repository plan. If starting empty, create the workspace.
2. Work in **phases**, but start implementing Phase 0 and Phase 1 immediately. Do not only write plans, pseudocode, or placeholder handlers.
3. After each phase, run relevant build, formatting, lint, and tests in the actual environment. Capture commands and results. If Windows is unavailable, distinguish compile verification from actual Windows integration testing, and provide exact PowerShell steps.
4. Keep `docs/IMPLEMENTATION_STATUS.md` with rows `Implemented / Tested / Partially implemented / Deferred / Blocked`, linking to exact modules and tests. Never hide unsupported features.
5. Every button must have working behavior, be disabled with a reason, or be explicitly marked not implemented. No fake progress bars, hard-coded benchmark numbers, or dummy background agents.
6. Prefer predictable correctness over a speculative claim of “10x speeds.” Measure real throughput and compare under equivalent conditions.
7. Use actively maintained stable dependencies after checking their current documentation and compatibility. As of the research date, relevant starting points are Tauri 2, Tokio 1, Reqwest 0.13, SQLx 0.9, Rust stable, React, Vite, TypeScript and Tailwind. Lock known-working versions in the lockfiles after validation; do not assume APIs from older tutorials.
8. Ask only when a decision prevents safe implementation. Otherwise adopt the defaults herein and keep moving.
9. Maintain a buildable main branch; prefer small testable commits and explicit migrations.
10. At the end of each phase, explain what is working, how to run it, what is untested, next phase, and how to reproduce failures.

## 1. Product positioning and non-goals
ForgeDM is a polished Windows download manager for direct HTTP/HTTPS files, later browser-triggered downloads, media workflows, and website asset collection. It should improve convenience, resilience, observability, and throughput when servers permit concurrency.

The following are **not guarantees**: faster than a saturated server/network; bypassing bandwidth restrictions, rate limits, paywalls, anti-bot systems, captchas, geo-restrictions, DRM, EME/Widevine/PlayReady, or browser protection systems; capturing every POST/blob/data/session-specific browser download; exact replication of IDM proprietary integrations. Be honest in the UI and documentation.

Do not use browser injection, TLS interception, custom root certificates, hidden proxies, kernel drivers, exploit-like interception methods, or antivirus disabling. Support content the user has rights to download; respect website access controls and rate limits.

## 2. Supported environment and cost
- Windows 11 x64, native user-level install; optional portable zip once the installer works.
- Default folder: user's Windows Downloads folder (resolve via Windows known folders API, not hard-coded English path).
- UI scales correctly at 100%, 125%, 150%, 200% DPI, light/dark themes, keyboard and screen-reader navigation.
- Build tools: latest compatible stable Rust toolchain, Visual Studio C++ Build Tools/MSVC, Node LTS, pnpm, Tauri 2 Windows prerequisites, WebView2 where required.
- Install with Tauri NSIS; install without Administrator privilege where technically possible; verify uninstall removes app registrations but **does not delete downloaded files**.
- Background agent autostarts at current-user sign-in **only if the user opts in**. Windows sign-in is required for this user-mode model; do not imply it downloads before login or while powered off.
- No cloud database or backend; zero hosted service dependencies for the core product.

## 3. Architecture: separate long-lived core from presentation
Use a Rust workspace roughly as follows; adjust only with a documented reason:

```
ForgeDM/
  Cargo.toml
  crates/
    forgedm-core/         # download task model, scheduler, transfer algorithm, rate limiter
    forgedm-storage/      # SQLx SQLite, schema, migrations, checkpoint journal
    forgedm-agent/        # per-user background binary, IPC server, process lifetime
    forgedm-protocol/     # typed IPC requests/responses/events, version compatibility
    forgedm-native-host/  # Chromium Native Messaging binary (stdin/stdout framed JSON)
    forgedm-cli/          # optional debugging/user CLI
    forgedm-platform/     # Windows known folders, MOTW, notifications, credentials, autostart
    forgedm-media/        # optional yt-dlp / ffmpeg adapter, never part of core HTTP loop
  apps/
    desktop/              # Tauri 2 shell + Vite/React/TypeScript frontend
  extensions/
    chromium/             # Chrome & Edge Manifest V3 extension
  tests/
    fixtures/             # local HTTP test server supporting malformed/normal responses
    integration/
  installers/
    windows/              # NSIS custom steps, host registration/unregistration
  docs/
    architecture.md
    development.md
    security.md
    browser-extension.md
    IMPLEMENTATION_STATUS.md
```

**Data plane:** forgedm-agent owns the Rust Tokio runtime, transfer queue, DB, metadata and disk writes. Desktop UI and browser extension are clients; the transfer does not stop if the window closes. Only one agent instance per Windows user. Avoid running the engine redundantly in both app and native host.

**Control plane:** local-only versioned, length-framed JSON or MessagePack messages over Windows named pipes (e.g. `\\.\pipe\ForgeDM-<user-sid>`). Apply an explicit restrictive security descriptor/DACL, allow the current user and system only, deny network access, validate message sizes, authenticate clients or issue an appropriate per-user installation token if needed, and handle IPC reconnects. Do not expose an unauthenticated localhost HTTP server. Frontend talks to Tauri invoke handlers; Tauri calls agent IPC. Native host bridges approved browser messages to the agent. CLI uses the same protocol.

**Lifecycle:** installer registers agent launch mechanism and native messaging host. UI can bootstrap agent. Native host can bootstrap it idempotently if not running. Close-window defaults to hide-to-tray; `Exit UI` leaves agent running (make that distinction visible); `Stop background engine` pauses/queues tasks safely. After agent crash/restart, restore checkpointed queue and resume when eligible.

**State management:** all mutable transfer state is authoritative in the agent, not frontend React state. UI receives events/snapshots (coalesced to roughly 4 updates per second) and reconnects automatically. Cancellation tokens, bounded channels, semaphores and backpressure must be used instead of unbounded tasks/queues.

## 4. HTTP/HTTPS downloader: functional specification
### 4.1 Capability probing
- Accept normal URLs, redirects, clipboard paste, custom filename/folder, headers, optional referrer and explicitly supplied credentials.
- Reuse one configured Reqwest client per profile/target policy; use connection pooling, TLS verification, configurable connect/read/idle/overall timeouts, HTTP/1.1 and HTTP/2 where compatible.
- Probe via `HEAD`; if HEAD fails, is forbidden, or inconclusive, use a bounded `GET` with `Range: bytes=0-0`. Do not download the entire file as a probe.
- Inspect final URL, HTTP status, Content-Length, Content-Type, Content-Disposition filename, ETag, Last-Modified, Accept-Ranges; **verify actual `206` and valid `Content-Range`** rather than trusting Accept-Ranges alone.
- For ranged binary downloads, request `Accept-Encoding: identity` and disable automatic transparent decompression as needed so offsets refer to stored bytes.
- If size is unknown or range unsupported, use exactly one sequential stream; never present pause/resume as reliably available for a nonresumable stream.
- Detect servers that return `200` to a `Range` request, change validators, provide mismatching ranges/lengths, or change data mid-download. Abort segmented writing and safely restart or ask for user decision instead of silently corrupting the file.
- For resume, use strong ETag with `If-Range` where available; otherwise a suitable Last-Modified validator. Weak ETags are not valid strong resume guarantees. If validators are absent, explicitly use safe fallback policies and optionally byte sampling, never blind append by default.
- Handle 200, 206, 301/302/303/307/308, 401/403, 404, 408, 416, 429, 500/502/503/504, redirects (bounded), timeout, DNS failures, TLS errors, connection reset, broken Content-Length, and no disk space. Honor Retry-After with bounded exponential backoff + jitter; no retry storm.
- Auth/cookie/header handling must respect redirects and origin boundaries; do not forward Authorization or sensitive headers to unrelated origins.

### 4.2 Multi-segment transfer
- Default adaptive mode: ~4 initial segments for large eligible resources, up to 8 per download and 8 per host (user configurable), with a global connection budget (e.g. 32). Start single-stream on small objects; minimum segment size initially 4-8 MiB, then tune based on measured throughput.
- Use disjoint byte ranges. Split the largest *remaining* eligible range when another worker becomes available and it is beneficial. Never overlap ranges or duplicate file writes. Do not confuse multiple parallel single-range GETs with HTTP multipart/byteranges responses.
- Use 64-bit byte offsets and handle files greater than 4 GiB. Bounded buffers (e.g. 64-512 KiB), bounded disk-writer queue, backpressure and no whole-file buffering.
- Use a preallocated or staged `.forgedm.part` file only where safe; each worker writes at fixed offsets via a dedicated blocking disk I/O pool or appropriate Windows file I/O API. Avoid synchronous file writes on Tokio event-loop threads.
- Save per-segment durable checkpoints (byte start/end, safely committed offset, validator, state, retry count) at a controlled cadence. Crash recovery must not mistake unflushed bytes for committed data. Sync/checkpoint ordering must be documented and tested.
- Once all expected bytes are verified and flushed, perform optional SHA-256 checksum verification (mandatory if an expected checksum is supplied), then atomic rename/move within the same filesystem. Deal with filename collisions: ask, rename, overwrite with confirmation, or skip; default safe rename.
- Derive progress, ETA (EWMA), instantaneous/smoothed speed, per-connection speed, retry count and real completion percentage from actual transferred bytes. Unknown length displays indeterminate progress, not fake percentages.
- Support pause/resume/cancel/delete-from-list/delete-partial-file separately with confirmation where destructive.
- Changing per-download connection count takes effect safely at run-time, without losing progress.

### 4.3 Resource policies
- Global bandwidth cap, per-download bandwidth cap, optional per-host cap, burst-safe token-bucket rate limiting.
- Max concurrently active downloads default 3; global and per-host active connection budgets; fairness so a large item cannot starve the queue.
- Priority: High/Normal/Low; manual queue reordering; one-click pause all/resume all.
- Per-host connection limits and cooldown on 429/503; do not circumvent site rate limits.
- Disk space checks, filesystem errors, Windows path length support, locked files and removable drive loss; actionable error messages.
- Categorise downloads by file type and user rules; store source, final URLs (sensitive at rest), file metadata, timestamps and history.
- Schedule once/recurring, quiet hours, only when plugged in / unmetered connection where Windows APIs allow, and optional completion action such as notification or sleep/shutdown **behind an explicit confirmation and disabled by default**.

## 5. SQLite persistence and file safety
Use SQLite WAL, foreign keys, transactional schema migrations, and one authoritative DB owner (agent). Suggested tables:

- `downloads(id, original_url_encrypted, final_url_encrypted, display_name, target_dir, mime, expected_size, etag, last_modified, checksum_alg, checksum_value, state, priority, category, created_at, updated_at, completed_at, last_error)`
- `segments(id, download_id, start_offset, end_offset_exclusive, committed_offset, state, retry_count, updated_at)`
- `queues(id, name, priority, schedule_id, enabled)` and `queue_items(queue_id, download_id, position)`
- `schedules(id, type, timezone, recurrence, start_at, end_at, policy_json)`
- `settings(key, typed_value)`; `host_policies(...)`; `categories(...)`; `download_events(...)`; `external_tools(...)`

Explicit state machine: `Queued -> Probing -> Downloading -> Verifying -> Completed`; from active states: `Pausing -> Paused`, `RetryWaiting`, `NeedsCredentials`, `NeedsUrlRefresh`, `Failed`, `Cancelled`. Validate legal transitions and make them idempotent. On startup reconcile DB with disk: `Downloading` becomes `Interrupted/Queued`, verify checkpoint/file length and server resource identity. Never silently overwrite already completed targets.

Sensitive auth tokens, signed URLs, proxy credentials and cookies must not be plaintext in logs, exports, diagnostics or routine DB fields; encrypt necessary persisted secrets using Windows DPAPI/Credential Manager. User's own settings/DB under `%LOCALAPPDATA%\ForgeDM\`, logs/caches separated and rotated. Default downloads in the Windows-known Downloads directory. Exports omit secrets unless user specifically opts into encrypted export.

## 6. Browser extension (Chrome + Edge first)
Use **Manifest V3**, least-privilege permissions, user-visible enable/disable, site allowlist/blocklist, per-extension settings and explanatory onboarding. Features:

1. Right-click a normal link: `Download with ForgeDM`.
2. Toolbar: send current URL / detected downloadable links, show agent connection status, manage capture on current domain.
3. Optional automatic handoff for eligible standard HTTP[S] downloads via `chrome.downloads.onCreated` and `chrome.downloads.cancel` only after agent successfully acknowledges and persists the job. Validate actual behavior under Chrome/Edge and avoid duplicates; if handoff fails, let the browser own the download. Do not claim it can intercept all downloads.
4. Rules for MIME types/extensions, minimum file size if known, exclusions, and user choice for risky/sensitive types.
5. Native Messaging bridge: separate `forgedm-native-host.exe`, framing = 32-bit native-endian byte count + UTF-8 JSON on binary stdin/stdout. **Never write logs to stdout.** Enforce allowed extension origins. Windows installer creates correct `HKCU\Software\Google\Chrome\NativeMessagingHosts\<host>` and `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\<host>` registration entries; removes only its own registrations on uninstall. Configure actual stable unpacked/published extension IDs in allowed_origins (no wildcards). Chrome and Edge may have different IDs.
6. For authenticated downloads, do not copy browser cookies by default. Implement per-site, explicit opt-in, explain sensitivity, transmit minimum necessary data, encrypt at rest, and never log cookie/header values. Non-GET POST-body downloads, blob URLs and service-worker-generated files may not be reproducible by the agent; offer browser fallback.
7. No broad page-content scraping, arbitrary JS injection, hidden network capture, TLS interception or disabling Safe Browsing.
8. Manual `Load unpacked` installation instructions, plus stable development extension IDs and native-host pairing diagnostic screen.

Optional Firefox support can follow after Chromium is working and tested.

## 7. Video/media module
Provide a separate optional `yt-dlp` adapter. For supported, lawful, non-DRM media URLs:
- Paste page URL -> probe metadata/formats -> choose quality/resolution, codec/container, audio track, subtitles -> start -> show real progress/status -> optional FFmpeg merge/remux/audio extraction and thumbnail/metadata embedding when supported.
- Detect locally installed `yt-dlp.exe`, `ffmpeg.exe`, `ffprobe.exe` and required JS runtime such as Deno for applicable sites. Show component health/version/location. Explain what is missing, do not silently fetch executables.
- Spawn the configured executable directly with argument arrays (never shell concatenation); set safe timeouts, cancellation/child-process termination, captured stderr and structured JSON metadata from yt-dlp where supported.
- yt-dlp supported websites change often and are not a compatibility guarantee. Some sites require `yt-dlp-ejs` plus a JS runtime. Provide opt-in manual update workflow using official sources plus checksum/signature verification if available.
- Clearly warn if content is DRM-protected, unavailable, geo-blocked or requires authorization. Do **not** implement DRM bypass, credential theft or paywall circumvention.
- A browser "download video" control can offer supported page analysis via explicit user action. Do not promise universal IDM-style floating overlays.
- Keep third-party tools optional and separately licensed; yt-dlp source may be Unlicense while common Windows bundled executables include GPLv3+ components; FFmpeg license depends on build. Document redistribution obligations if packaging tools.

## 8. IDM parity roadmap (implement in this order)
**P0 — Reliable core, must pass before UI polish**
- HTTP/HTTPS single download; correct files and SHA-256 validation; live progress; retry; clean cancellation.
- Reliable HTTP byte-range resume and robust handling of non-range servers.
- Persistence through process exit and Windows reboot/sign-in.
- Multiple parallel tasks, adaptive segments, rate caps and safe filename handling.
- CLI/local test harness and logs; SQLite migrations and fault-injection tests.

**P1 — Production desktop experience**
- Tauri desktop: add links, clipboard, download list, pause/resume/cancel/retry, destination selection, queue, priority, sorting/filter/search, categories, settings, real-time speeds and ETA, notifications, dark/light, keyboard shortcuts, tray and background engine.
- Windows installer, uninstall, auto-start opt-in, context menu where practical, diagnostics/export, update checker that never silently installs.
- Chrome/Edge extension with Native Messaging; explicit/optional handoff, right-click download and host pairing diagnostics.

**P2 — Media and user automations**
- yt-dlp/FFmpeg integration with metadata and format selector.
- Batch URL import, numbered URL templates, checksum-file import (SHA-256), filename/category rules, scheduled queues, archive/ZIP content listing without full extraction where technically possible, deduplication by checksum or robust identity.
- Download complete hooks restricted to user-allowlisted safe actions, no arbitrary remote code execution.

**P3 — Extended IDM-like tools**
- Filtered website asset grabber / offline crawler: same-site by default, adjustable depth, file type, max pages, max data, max rate, robots.txt policy and manual user confirmation; rewrite relative links best effort; don't claim complete SPA mirroring.
- FTP / FTPS adapter, with explicit security modes and resumability based on server support. Evaluate stable Rust FTP libraries first; do not pretend Reqwest itself handles FTP.
- Proxy support HTTP/SOCKS; authenticated proxy via supported mechanisms; NTLM/Kerberos/enterprise auth only when supported by a tested stack, not as unverified checkboxes.
- Optional Firefox browser integration, checksum manifests, better accessibility/localisation, site-specific rules, and profiles.

**P4 — Differentiators (optional, opt-in)**
- Adaptive connection tuning based on actual throughput, error rate, host limits and disk load; conservative defaults and measured A/B benchmarks.
- Smart disk-space forecasting: pause before disk fills and suggest a different destination.
- Self-healing verification: when a trusted chunk manifest exists, re-download only corrupt chunks; with only a whole-file hash, re-download the full file when repair cannot be proven.
- Source re-authentication workflow: paste refreshed signed URL and revalidate file identity before resuming.
- File deduplication, content fingerprint lookup, full-text download-history search, privacy-preserving optional clipboard watcher, exportable diagnostics, per-site speed analytics.
- Optional authenticated local/Tailscale-only remote queue API **disabled by default**, strongly authenticated, no public binding or inbound ports by default. Never add remote access without an explicit threat model and user opt-in.
- Optional multi-source mirror downloading only when sources are user-provided and byte-identical (same hash/strong validation), not speculative cross-domain source switching.

## 9. Premium desktop experience: visual design, interaction, motion and accessibility

### 9.1 Product feel and design direction
ForgeDM should feel like a professionally designed, premium Windows 11 utility that could plausibly be a paid commercial desktop product, despite being entirely free for personal use. Do **not** interpret "functional UI" as enough. Visual quality is a release gate; equally, the UI must never hide broken core functionality.

Design direction: original product language with the polish, tight information hierarchy and responsive interactions of high-end productivity apps. Draw inspiration from Fluent 2's platform ergonomics, but do not copy IDM, Linear, Raycast, Arc, or any product's proprietary artwork/layout/trade dress. Aim for purposeful, restrained and distinctive rather than heavy neon gamer visuals.

Use **dark mode as the initial default**, with a genuinely designed light theme and a setting to follow Windows. Suggested palette tokens (adjust as a coherent system): background `#0D1016`, elevated surface `#151A23`, floating surface `#1C2330`, primary text `#F2F5FA`, muted text `#98A4B5`, border `rgba(255,255,255,0.08)`, accent `#6C8CFF` with complementary cyan `#49C7DA` used sparingly. Success/warning/error are semantic, accessible and never only conveyed by colour. Set these as CSS design tokens rather than scattering hex literals. User-selectable accent colour is a nice-to-have, not a core blocker.

Typography: Windows-friendly `Segoe UI Variable` when available, with a dependable fallback, tabular numeric figures for speeds/sizes/ETAs. Consistent 4/8px-derived spacing scale, 10–14px radii for surfaces, subtle separators, restrained depth and purposeful whitespace. Native-feeling keyboard focus, scrollbars, window controls, drag regions and DPI adaptation. Avoid giant cards that waste space in desktop utility workflows.

### 9.2 Screens and information architecture
- **Overview / All Downloads:** top compact toolbar and search; sidebar for All, Active, Queued, Paused, Completed, Failed, Video, Scheduler and Settings. Main area has an information-dense, resizable download table/list with custom sort, column visibility, multi-select, bulk actions and context menus. A collapsible detail inspector lives at right or bottom rather than navigating away from the queue.
- **Active downloads:** prioritise filename, source, actual bytes, progress, smoothed speed, ETA, status and pause/resume. Display aggregate throughput, number of active tasks and queue health in a restrained status strip. Show per-download segment activity only in expanded details, never as distracting animation in every row.
- **Download inspector:** overview, source and HTTP metadata, file integrity/validator status, segment map/timeline, retry history, failure reason, destination controls and copy-debug-info. Use charts only if backed by real telemetry.
- **Add download modal / command palette:** paste or drag URL; editable filename/path; optional headers/referer/auth where supported; capability probe and transparent indication of resume/range support; enqueue now or schedule. Ctrl+N opens; Ctrl+K opens command palette (search actions, downloads and settings).
- **Scheduler and queue:** calendar/time-based jobs, time windows, global and per-host bandwidth policies, priorities and queue ordering, with straightforward rule explanations. Implement only after the engine supplies the corresponding capabilities.
- **Settings:** General, Downloads, Acceleration, Browser Integration, Media Tools, Appearance, Privacy & Security and Diagnostics. Every setting states what it changes; unimplemented options are omitted or visibly disabled with a clear reason.
- **Empty, waiting, offline, error and success states:** distinct professionally authored states with relevant action; no gratuitous confetti or generic filler marketing copy.

### 9.3 Component system and implementation
Use React + TypeScript + Tailwind + shadcn/ui/Radix primitives and lucide-react icons, with a **small custom design system** for download-specific components. Consider `motion` (Motion for React, current stable package and API verified at implementation time) for small, stateful transitions; prefer native CSS for simple hover/focus transitions. For huge lists, use a virtualizer such as `@tanstack/react-virtual`, checking current compatibility before adoption. Avoid large charting or visual-effects dependencies unless they solve a genuine use case.

Create reusable components such as `AppShell`, `SidebarNav`, `GlobalToolbar`, `QueueToolbar`, `DownloadList`, `DownloadRow`, `ProgressIndicator`, `SpeedSparkline`, `DownloadInspector`, `AddDownloadDialog`, `CommandPalette`, `StatusPill`, `ConnectionSegmentMap`, `ToastStack`, `EmptyState` and `SettingsSection`. Build Storybook or a lightweight equivalent component-development sandbox only if it materially speeds QA; don't add it just for show.

Every interactive element must have a clear enabled/disabled state and real action. Loading/progress indicators reflect authentic engine/IPC state. If total bytes are unknown, show indeterminate progress rather than fabricate a percentage. If there is no ETA, show `Estimating…` or `Unknown`, not a fake number.

### 9.4 Animation and microinteraction specification
Motion should be **visible and premium, but controlled and performant**, never excessive or purely decorative. Recommended defaults (adjust after real Windows testing):

| Interaction | Motion | Approximate duration |
| --- | --- | --- |
| Sidebar navigation | Soft content fade/translate of 4–8px; active-item indicator glides | 140–200ms |
| Modal / Add Download dialog | Opacity + scale 0.98 → 1 with subtle elevation | 160–220ms |
| Inspector opening | Slide/fade with animated width only if smooth; otherwise transform-based overlay | 180–250ms |
| Download row enters queue | Fade + small translate, with stable list reflow | 160–220ms |
| Pause → Resume / Complete / Failed | Controlled icon/status crossfade; one-time accent feedback on completion | 120–220ms |
| Context menu / popover / tooltip | Very short fade/scale | 90–150ms |
| Hover, press and focus | Subtle tonal shift/press, no dramatic bounce | 80–140ms |
| Download progress | Smooth visual interpolation between real engine snapshots; never advance independently of actual bytes | 150–250ms |
| Toast entry/exit | Slide/fade with predictable focus handling | 140–220ms |

- Use modern spring easing only where it improves the feel (menus, selection indicator). No nonstop glow, particle explosions, animated gradient backgrounds, heavy glass blur across the full app, scroll-jacking, shaking panels or perpetual skeletons.
- Animate compositor-friendly `transform` and `opacity` when practical. Avoid layout-triggering animation on every transfer update or every row. Never animate thousands of list cells simultaneously.
- Honour `prefers-reduced-motion` and offer an explicit **Reduce animations** toggle. Reduced motion should disable spatial movement and keep necessary state feedback immediate/brief.
- Coalesce high-frequency engine updates to a GUI refresh rate of ~4 per second, except interaction acknowledgements; do not trigger a full table rerender at chunk frequency. Keep animation separate from IO and Rust transfer scheduling.
- Respect battery/low-power conditions, background window minimization and inactive tabs where feasible: pause nonessential animations, not actual downloads.
- Do not use animation to conceal latency, missing features, failed commands, or speculative progress. Real state always wins.

### 9.5 Responsiveness, density and ergonomics
- Optimise primary experience for 1080p and 1440p monitors, including 125% / 150% Windows scaling. App remains usable at approximately 900×600 logical pixels, with no clipped primary controls or modal content.
- Offer compact and comfortable row density settings. Queue supports thousands of historical records through virtualization, pagination or equivalent bounded rendering; no noticeable freeze while rapidly updating many active jobs.
- Keyboard-first workflows: Ctrl+N add, Ctrl+K palette, Ctrl+F search, Space pause/resume selected, Delete remove with confirmation, Ctrl+A select all in the queue, Escape dismiss transient panels. Shortcuts must not interfere with text fields and use standard Windows conventions.
- Screen-reader labels, focus restoration after modal dismissal, semantic contrast, keyboard reachable menus, visible focus rings and colour-independent state indicators. Aim for WCAG 2.2 AA where applicable to desktop web content.
- Drag/drop URL text and `.txt` URL lists and supported checksum files, with a visible drop affordance only during a valid drag. Avoid intercepting arbitrary local files as executable commands.

### 9.6 Performance budgets, visual QA and release gate
- Visual updates must remain responsive during 8+ concurrent transfers on the target Windows hardware; benchmark UI and core separately. Aim for smooth 60fps interactions on typical Windows 11 desktops rather than claiming guaranteed FPS in all environments.
- Keep memory bounded independently of download file size. Document baseline idle UI memory and memory under a large virtualized queue; investigate major regressions.
- Screenshot-based design reviews for: desktop empty state, many active downloads, one failed download with expanded inspector, Add Download dialog, Settings, dark mode, light mode, 125% and 150% scaling, narrow window, reduced motion. Automate where practical and record actual screenshots in `docs/ui-review/` without fabricating results.
- **Quality gate before calling Phase 2 complete:** consistent spacing/typography, no horizontal overflow, all core controls operable, real state and progress, theme/scale/keyboard tested, no inaccessible modal, animated transitions obey reduced motion, queue updates do not stutter. Do not call a default shadcn template "premium" without visually reviewing it on Windows.
- For UX iteration, first create a small original design-token sheet and a polished shell + three representative states (active, paused, completed), then implement functional screens. This UI proof should happen **during Phase 2**, after the real Phase 1 engine is verified; it must not block engine correctness.

## 10. Security and privacy non-negotiables
- Never disable TLS verification, Windows Defender, SmartScreen, or browser safeguards to make a feature work.
- Integrate Windows Attachment Manager (`IAttachmentExecute` where appropriate) / Mark-of-the-Web for downloaded internet files. Preserve OS security warnings; never auto-run downloaded executables.
- Sanitize Content-Disposition and URLs: reject traversal, path separators, reserved Windows filenames, trailing dots/spaces, alternate data streams, malicious Unicode filename patterns, unexpected absolute paths, symlink/hardlink overwrite and unsafe overwrite races. Display original name but write safe name.
- Validate IPC origin/permissions and limit message lengths. Electron/Node exposed APIs are not permitted; use scoped Tauri commands/capabilities and a strict CSP.
- Default telemetry off (prefer none), no registration, no secret upload, no AI/cloud requirement.
- Only install native-host registry keys after the user enables integration; uninstall cleans them.
- Protect secrets at rest with DPAPI/Credential Manager, avoid secrets in logs/CLI args, apply least privilege, and document threat model.
- Browser host cannot run arbitrary commands from a webpage. Its protocol accepts a small set of validated actions (enqueue URL, query status, ping), with domain/origin checks.
- Avoid trusting web-provided filenames, checksums, redirects or MIME types; treat all as untrusted input.
- Respect reasonable rate limits and HTTP Retry-After; protect against request amplification.

## 11. API/IPC contract (typed and versioned)
Example commands: `Handshake`, `EnqueueUrl`, `EnqueueBatch`, `ListDownloads`, `GetDownload`, `Pause`, `Resume`, `Cancel`, `Retry`, `UpdatePriority`, `UpdateLimits`, `ChooseDestination`, `GetSettings`, `UpdateSettings`, `GetDiagnostics`, `ShutdownAgentSafely`. Each request has `protocol_version`, `request_id`, `action`, and typed `payload`. Each response has `request_id`, `ok`, `result` or structured `error(code, message, retryable, details)`.

Publish events: `DownloadAdded`, `DownloadChanged`, `ProgressSnapshot`, `DownloadCompleted`, `DownloadFailed`, `QueueChanged`, `AgentStatus`. On reconnect, client sends `GetSnapshot` before subscribing to incremental events; sequence numbers prevent stale UI. Use exhaustive Rust enums and TypeScript-generated or schema-validated types; test protocol compatibility.

## 12. Acceptance tests and benchmark methodology
A phase is complete only with reproducible evidence, not visual appearance.

**Local HTTP fixture server** (Python/Node/Rust, test-only) must simulate: HEAD disabled, no Content-Length, no range support, valid 206, server ignores range and returns 200, malformed Content-Range, wrong Content-Length, mutable ETag and changed content, 416, 429 with Retry-After, 503, redirect across hosts, dropped connection mid-chunk, slow streams, untrusted filenames, auth-required, concurrent clients and zero-byte file.

**Engine integration tests:**
- A reference file downloaded with 1, 4 and 8 workers has byte-identical SHA-256 and exact length.
- Force-kill the agent during multi-segment download, restart agent, verify no corruption and no completed-byte inflation.
- Change the ETag/data between runs; downloader rejects incompatible resumption.
- Server rejects ranges; downloader falls back to sequential and does not create gaps/overlap.
- Retry conditions respect Retry-After, backoff, cancellation and request limits.
- Pause/resume/cancel are race-safe and idempotent across UI/CLI/browser clients.
- Test >4 GiB offsets using a sparse local test fixture or generated file and verify exact size/checksums without shipping large test artifacts.
- Test Windows filename edge cases, duplicate target handling, disk-full/removable-drive error paths, SQLite migration, recovery and corrupt metadata.
- Cap memory independently of file size; no loading whole downloads into RAM.

**GUI/installer tests:** actual Windows 11 x64 build; app opens on first launch, agent survives UI close, tray actions work, reboots restore eligible queued jobs after user sign-in; install/uninstall/repair cleanly register/remove browser native hosts. Test Chrome and Edge download handoff and fallback without duplicates, with both installed and absent agent. Test high-DPI and keyboard navigation.

**Performance evaluation:** Compare direct browser download and ForgeDM on the same files/servers under equivalent conditions, with at least three runs per mode. Record throughput, latency, CPU, RAM, disk IO, source rate limits, network speed and file hashes. Include both range-capable and non-range-capable servers. Do not claim faster if statistically inconclusive or server-limited.

Commands/checks where applicable: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm lint`, `pnpm typecheck`, `pnpm test`, `pnpm tauri build`, Windows PowerShell smoke scripts; adapt to actual workspace.

## 13. Implementation phases and deliverables
**Phase 0:** scaffold Rust workspace, architecture doc, schema, typed IPC, local HTTP test fixture, toolchain detection, original app naming/icons. Produce successful builds of empty but connected agent+CLI and minimal Tauri shell.

**Phase 1:** real HTTP engine, safe sequential and multi-range transfers, resume/checkpoint, retry, SQLite, CLI, local fixtures and engine integration tests. Demonstrate a file download and SHA-256 match. Stop polishing UI until this is trustworthy.

**Phase 2:** implement the Section 9 design tokens and polished UI proof, then fully functional GUI backed by agent IPC, tasteful motion/reduced-motion support, system tray, app lifecycle, settings, download rows, queue, scheduler basic, notifications and installer. Run the Section 9 visual-quality gate and test persistence through UI close and agent restart.

**Phase 3:** Chrome + Edge MV3 extension, native host, registry setup, consentful handoff, filters and fallback. Actual manual Windows browser test and installation instructions.

**Phase 4:** optional yt-dlp/FFmpeg/Deno component health, media metadata, format picker, progress, outputs and failure handling. Document third-party binary licensing.

**Phase 5:** site grabber, FTP/FTPS, advanced authentication/proxies, usability, diagnostics, crash injection, performance tuning, optional advanced features.

After each phase provide (a) actual changed file tree, (b) working command, (c) test results, (d) known limitations, (e) next tasks. If you cannot execute Windows tests in your environment, include honest unverified status and runnable `scripts/windows-smoke.ps1`; do not invent passing outcomes.

## 14. First action, right now
Start by creating the repository with Phase 0 and a testable Phase 1 vertical slice, not by drafting a product pitch. Build:
1. Rust download agent with named-pipe IPC, SQLite persistence, `EnqueueUrl` and `GetDownload`.
2. CLI that submits a URL, displays true progress, and can pause/resume.
3. Real HTTP downloader that supports single-stream first, then valid byte-ranges/resume, with correct disk writes and SHA-256 comparison.
4. Local adversarial HTTP server fixture and automated tests.
5. Tauri/React UI showing tasks from real IPC.

Only proceed to browser integration and media extraction after the core tests pass. If any feature is technically impossible or unreliable under supported browser/Windows APIs, implement a transparent fallback and record it in `IMPLEMENTATION_STATUS.md`.

## Reference documentation
- IDM capabilities: https://www.internetdownloadmanager.com/features2.html
- HTTP Range / If-Range: https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/Range_requests
- Reqwest: https://docs.rs/reqwest/latest/reqwest/
- SQLx SQLite: https://docs.rs/sqlx/latest/sqlx/sqlite/
- Tauri: https://v2.tauri.app/ ; tray https://v2.tauri.app/learn/system-tray/ ; installer https://v2.tauri.app/distribute/windows-installer/
- Chrome downloads: https://developer.chrome.com/docs/extensions/reference/api/downloads
- Chrome Native Messaging: https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging
- Edge Native Messaging: https://learn.microsoft.com/en-us/microsoft-edge/extensions/developer-guide/native-messaging
- Windows named-pipe security: https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights
- Windows Attachment Manager: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-iattachmentexecute
- yt-dlp: https://github.com/yt-dlp/yt-dlp ; JS runtime/ejs https://github.com/yt-dlp/yt-dlp/wiki/EJS
