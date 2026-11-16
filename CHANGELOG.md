# Changelog

All notable changes to ForgeDM are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). While the major version is 0,
releases are published as pre-releases and minor versions may change behaviour.

## [Unreleased]

## [0.1.0] - 2026-11-16

First public pre-release: the download engine, the command line and a working desktop app.

### Added
- **Engine**
  - HTTP and HTTPS downloads (rustls only; HTTP/1.1; redirects followed and checked hop by hop).
  - Multi-connection downloads (up to 16) with segment splitting and work stealing.
  - Crash-safe resume: data is flushed and synced before each checkpoint commit, and resumes send `If-Range` with the stored validator.
  - Sample verification of the last 64 KiB before the resume point when the server gives no strong validator.
  - Automatic retries with jittered back-off, `Retry-After`, per-host cool-down and one safe re-resolve of expiring signed links.
  - Queue with priorities, up to 10 active downloads (default 3), a global speed limit, per-download speed and connection settings, and global and per-server connection budgets.
  - Pause, resume, cancel, retry, remove, Pause all and Resume all.
  - Free-space checks, optional expected SHA-256, and startup recovery of downloads interrupted while finishing.
- **Safety**
  - Never overwrites an existing file; collisions get a new name.
  - Mark-of-the-Web (Zone.Identifier plus the Windows attachment service) on every finished file.
  - Download URLs encrypted at rest with DPAPI; logs, UI, errors and CLI output show redacted URLs only.
  - Engine pipe restricted to the current user, with mutual identity checks between clients and engine.
  - A custom `--data-dir` must be private to the current user.
- **Apps**
  - `forgedm` command line: add (also from stdin), watch, list, get, pause, resume, cancel, retry, remove, limits, pause-all, resume-all, priority and agent control.
  - Desktop app (Tauri 2, React): add a link, live progress, per-download controls, priority and connection selectors, Pause all, Resume all, a Limits panel and plain-language status messages.
  - Per-user Windows installer (NSIS) that needs no administrator rights.

### Known limitations
- The installer is not code-signed, so SmartScreen may warn on first run.
- No browser integration, scheduler, tray icon, notifications or settings screen yet.
- The desktop app refreshes by polling once a second; live event updates come later.
- No cookies, custom headers or logins (servers that need a login show a clear message).

[Unreleased]: https://github.com/KartikCodes1/IDM-Clone/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/KartikCodes1/IDM-Clone/releases/tag/v0.1.0
