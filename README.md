# ForgeDM

A fast, careful download manager for Windows 11.

[![CI](https://github.com/KartikCodes1/IDM-Clone/actions/workflows/ci.yml/badge.svg)](https://github.com/KartikCodes1/IDM-Clone/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

ForgeDM splits a download over several connections, resumes it after a crash, reboot or dropped network without corrupting a byte, and keeps your files and links safe on disk. A small background engine owns the queue, the database and the files. The desktop app and the `forgedm` command line talk to it over a private, locked-down Windows pipe.

> **Status: early pre-release (0.1.x).** The core engine and a working desktop app are done and heavily tested. Browser integration, a scheduler and the polished UI are still to come. See [Roadmap](#roadmap).

## Features

**Downloading**
- HTTP and HTTPS (rustls TLS only).
- Up to 16 connections per download. Segments are split and handed to idle connections as they finish, and the file is assembled in place.
- Resume after pause, crash, kill or reboot from the last committed checkpoint. Data is written, flushed and synced before the checkpoint is committed, and the server's file is re-checked with `If-Range` before more bytes are accepted.
- When a server sends no strong validator, the last 64 KiB before the resume point are downloaded again and compared first, so a changed file is caught instead of silently spliced.
- Automatic retries with jittered back-off, `Retry-After` support and per-host cool-down. Expiring signed links are re-resolved once, safely.
- An optional expected SHA-256; a mismatch fails the download.

**Control**
- Queue with priorities (high, normal, low) and up to 10 downloads at once (default 3).
- A global speed limit, per-download speed and connection settings, and global and per-server connection budgets.
- Pause, resume, cancel, retry, Pause all and Resume all.
- Free disk space is checked before a download starts and again as it grows.

**Safety**
- Never overwrites an existing file; picks a new name instead.
- Every finished file gets the Windows "downloaded from the internet" mark (Mark-of-the-Web), so SmartScreen and Office Protected View still apply.
- Download links are encrypted at rest with Windows DPAPI. Logs, the UI and error messages show only redacted links (no tokens or signatures).
- The engine's pipe accepts only your own user account, and every client checks it is talking to the real ForgeDM engine before sending anything.
- Runs as a normal user and never asks for administrator rights.

## Install

1. Download `ForgeDM_<version>_x64-setup.exe` and `SHA256SUMS.txt` from the [latest release](https://github.com/KartikCodes1/IDM-Clone/releases).
2. Optional but recommended: verify it.
   ```powershell
   (Get-FileHash .\ForgeDM_0.1.0_x64-setup.exe).Hash.ToLower()   # must match SHA256SUMS.txt
   gh attestation verify .\ForgeDM_0.1.0_x64-setup.exe --repo KartikCodes1/IDM-Clone `
     --signer-workflow KartikCodes1/IDM-Clone/.github/workflows/release.yml `
     --source-ref refs/tags/v0.1.0 --deny-self-hosted-runners
   ```
   The second command checks that the installer was built by this repository's release workflow, from that release's tag, on a GitHub-hosted runner.
3. Run it. It installs for your user only, into `%LOCALAPPDATA%\ForgeDM`, with no admin prompt. The installer is not code-signed yet, so SmartScreen may say "Windows protected your PC": choose **More info**, then **Run anyway**.

## Use it

**Desktop app:** start ForgeDM from the Start menu, paste a link, choose a folder and press Add. Each row shows progress, speed and time left, with buttons to pause, resume, cancel, retry and open the folder. The toolbar has Pause all, Resume all and the Limits panel.

**Command line:** `forgedm.exe` is installed next to the app in `%LOCALAPPDATA%\ForgeDM`. The installer does not change your PATH; add that folder to your user PATH, or call it by its full path (`& "$env:LOCALAPPDATA\ForgeDM\forgedm.exe" list`). The examples assume it is on PATH:

```powershell
forgedm add https://example.com/big.iso --watch       # add and follow until done
forgedm add - --dir D:\Downloads                      # read a token-bearing URL from stdin
forgedm list
forgedm get <id>                                      # state, bytes, segments
forgedm pause <id>
forgedm resume <id>
forgedm limits --max-active 3 --speed 2097152         # 2 MiB/s total; `--speed off` removes it
forgedm priority <id> high
forgedm agent stop
```

Run `forgedm <command> --help` for every option. Data and logs live in `%LOCALAPPDATA%\ForgeDM`.

## Build from source

Requirements: Windows 10/11 x64, [rustup](https://rustup.rs) (the toolchain pinned in `rust-toolchain.toml` installs itself), Visual Studio 2022 Build Tools with the C++ workload, the WebView2 runtime (built into Windows 11), Node 24.15+ or 26, and pnpm 10.29.2.

```powershell
pnpm install --frozen-lockfile
pnpm --filter @forgedm/desktop build          # the desktop crate embeds this frontend
cargo build --workspace --locked
cargo test --workspace --locked
pnpm --filter @forgedm/desktop run package    # installer -> target\release\bundle\nsis\
```

More in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## How it is built and tested

- Rust workspace: `forgedm-core` (engine), `forgedm-agent` (background engine process), `forgedm-cli`, `forgedm-ipc` (pipe protocol), `forgedm-storage` (SQLite), `forgedm-platform` (the only crate with `unsafe` Win32 code) and `forgedm-protocol` (wire types). Desktop app: Tauri 2, React 19 and TypeScript.
- About 800 Rust tests and 126 desktop tests run on every push, against an adversarial test server that misbehaves on purpose (wrong ranges, changed files, dropped connections, redirects, rate limits).
- A crash matrix kills the engine at five critical points and checks that every file still finishes with the exact SHA-256. A first-proof script kills a multi-connection download partway and checks that the resumed file is byte-identical.
- Design and security notes: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md). Test evidence with real commands and exit codes: [docs/TEST_EVIDENCE.md](docs/TEST_EVIDENCE.md).

## Roadmap

- **Next:** polished UI, system tray and notifications, a settings screen, live event updates instead of polling, uninstall cleanup and optional start with Windows.
- **Later:** browser extension and native messaging host, scheduler and categories, cookies and custom headers, media sites, proxies and FTP.

## Security

Please report vulnerabilities privately; see [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)
