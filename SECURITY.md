# Security Policy

## Supported versions

Only the latest released version of md-viewer receives security fixes.

## Reporting a vulnerability

Please report vulnerabilities privately using GitHub's
[private vulnerability reporting](https://github.com/daemonhorn/md-viewer/security/advisories/new)
for this repository. Do not open a public issue for security problems.

You can expect an acknowledgement within 7 days. Please include the affected
version, platform, and a minimal reproducer (for example a Markdown file).

## Scope

Of particular interest: HTML/script injection through rendered Markdown
(sanitizer bypass), unsafe link or file handling, installer behaviour
(file association changes, privilege use), and release-integrity issues.

## Verifying releases

Every release artifact is signed with Sigstore cosign (keyless) and has a
GitHub build-provenance attestation. See [docs/RELEASING.md](docs/RELEASING.md)
for verification commands.
