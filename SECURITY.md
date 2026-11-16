# Security policy

## Supported versions

Only the latest release receives fixes. ForgeDM is pre-1.0, so please update to the newest version before reporting.

## Reporting a vulnerability

Please do **not** open a public issue for a security problem.

Report it privately through GitHub: open the repository's **Security** tab and choose **Report a vulnerability** ([direct link](https://github.com/KartikCodes1/IDM-Clone/security/advisories/new)). Include what you found, how to reproduce it, the ForgeDM and Windows versions, and the impact you expect. Redact any real download URLs that carry tokens.

You should get a first reply within 7 days. Fixes are released as soon as practical, and reporters are credited in the release notes unless they prefer otherwise.

## Scope

In scope: the ForgeDM engine (`forgedm-agent`), the `forgedm` command line, the desktop app and the installer built by this repository's release workflow.

Examples of what matters most:
- writing outside the chosen download folder, overwriting an existing file, or a finished file missing its Mark-of-the-Web;
- another local user or a low-integrity process controlling the engine through its pipe;
- download URLs, tokens or signatures appearing in logs, the UI, errors or the database in plain text;
- a server making ForgeDM corrupt a file without failing the download.

The design and its known limits are documented in [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md).

## Verifying releases

Each release lists SHA-256 checksums in `SHA256SUMS.txt`, and both the installer and the checksum file carry GitHub build provenance. Check an installer with:

```powershell
gh attestation verify .\ForgeDM_<version>_x64-setup.exe --repo KartikCodes1/IDM-Clone `
  --signer-workflow KartikCodes1/IDM-Clone/.github/workflows/release.yml `
  --source-ref refs/tags/v<version> --deny-self-hosted-runners
```

This checks that the file was built by this repository's release workflow, from the release's tag, on a GitHub-hosted runner. Releases are only built from commits on `main` that already passed CI.
