# Releasing md-viewer

## Cutting a release

0. One-time repository setup (manual, cannot be done from the workflows):
   see [Required repository settings](#required-repository-settings).
1. Make sure `main` is green in CI (including `cargo deny`).
2. Bump `version` in the workspace `Cargo.toml` (and update `Cargo.lock`).
   The release workflow fails if the tag does not match this version.
3. Commit, push to `main` (via PR), then tag **that commit on main** and push:

   ```sh
   git tag -s v0.1.0 -m "md-viewer 0.1.0"   # or: git tag v0.1.0
   git push origin v0.1.0
   ```

   Tags containing a `-` (e.g. `v0.1.0-rc1`) are published as pre-releases.
4. The `Release` workflow then:
   - `gate`: fails unless the tagged commit is an ancestor of `origin/main`,
   - builds Windows (MSVC), Linux (ubuntu-22.04, glibc) and FreeBSD 14 binaries
     with a pinned Rust toolchain (`RUST_VERSION` in `release.yml`) and no build
     cache,
   - generates the CycloneDX SBOM in a separate job (`sbom`) that has no
     signing permissions (it compiles `cargo-cyclonedx` from source),
   - waits for approval of the `release` environment, then in `sign`:
     verifies that every expected artifact exists, creates `SHA256SUMS`, signs
     and attests (this job never builds or installs anything from source),
   - packages `md-viewer-<ver>-x86_64-pc-windows-msvc.zip`,
     `md-viewer-<ver>-x86_64-unknown-linux-gnu.tar.gz`,
     `md-viewer-<ver>-x86_64-unknown-freebsd.tar.gz`,
   - signs every file and `SHA256SUMS` with cosign keyless (`<file>.bundle`),
   - creates a GitHub build-provenance attestation,
   - after a second `release` environment approval, creates the GitHub
     release (the only job with `contents: write`).

   The SBOM is `md-viewer-<ver>.cdx.json`. Linux and Windows archives and the
   SBOM are mandatory: `sign` fails if any is missing. The FreeBSD archive is
   optional during the beta, controlled by the `ALLOW_MISSING_FREEBSD`
   constant at the top of `release.yml` (`"true"` now). When it is missing,
   the run shows a warning annotation and a job-summary banner, and
   `install.sh` on FreeBSD prints that no build exists. Set the constant to
   `"false"` (and remove the FreeBSD `continue-on-error`) before 1.0.
5. Verify the published release using the commands below, then test the
   installers (`installers/install.sh`, `installers/install.ps1`).

If a release build fails, delete the tag (`git push --delete origin vX.Y.Z`),
fix, and re-tag. Never reuse a version that was successfully published.

## Required repository settings

These are manual GitHub settings; nothing in the repository can apply them.
Releases are only as trustworthy as these are.

1. **Environment `release`** (Settings -> Environments -> New environment):
   - add **required reviewers** (at least one maintainer other than the person
     pushing the tag, if possible) and enable "Prevent self-review",
   - restrict **deployment branches and tags** to the tag pattern `v*`,
   - no environment secrets are needed (signing is keyless).
   The `sign` and `release` jobs both reference it and each wait for approval.
2. **Tag ruleset** (Settings -> Rules -> Rulesets -> New tag ruleset):
   target tags `v*`; enable "Restrict creations" (only maintainers/release
   role), "Restrict updates", "Restrict deletions" and "Block force pushes".
   A published version must be immutable.
3. **Branch ruleset for `main`**: require pull requests, require the CI status
   checks (fmt + clippy, test, MSRV, cargo-deny, shellcheck, PSScriptAnalyzer,
   installer tests), block force pushes and deletions, require linear history
   if desired. The release `gate` job additionally refuses tags that are not
   on `main`.
4. **Actions settings**: workflow permissions default to read-only; allow only
   selected actions (GitHub-owned plus the SHA-pinned ones used here); require
   approval for workflows from outside contributors.
5. Enable private vulnerability reporting, Dependabot alerts and secret
   scanning (see `SECURITY.md`).

## Verifying a release (users)

Download an artifact, `SHA256SUMS`, and the matching `.bundle` files.

### Checksum

```sh
sha256sum -c SHA256SUMS --ignore-missing      # Linux
sha256 -q <file>                              # FreeBSD: compare with SHA256SUMS
```

### cosign (Sigstore, keyless)

```sh
cosign verify-blob \
  --bundle md-viewer-0.1.0-x86_64-unknown-linux-gnu.tar.gz.bundle \
  --certificate-identity 'https://github.com/daemonhorn/md-viewer/.github/workflows/release.yml@refs/tags/v0.1.0' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  md-viewer-0.1.0-x86_64-unknown-linux-gnu.tar.gz
```

Use the exact identity of the tag you are verifying (replace `v0.1.0`). Do the
same for `SHA256SUMS` / `SHA256SUMS.bundle`.

### Installers

`installers/install.sh` (Linux/FreeBSD) and `installers/install.ps1` (Windows)
download a release, verify the cosign signature against the exact identity
above (bound to the requested version), verify the checksum, check that the
binary reports the requested version, and only then run `md-viewer install`.

- Version: pass `--version X.Y.Z[-pre]` (`-Version` on Windows). Without it the
  newest **stable** release is used. GitHub's "latest" excludes pre-releases,
  so while only pre-releases such as `0.1.0-beta1` exist you must pass
  `--version 0.1.0-beta1`, or `--pre` (`-Pre`) to take the newest release
  including pre-releases.
- `--skip-signature-check` (`-SkipSignatureCheck`) is unsafe. It asks for an
  interactive `yes`, or requires `--i-know-this-is-unsafe`
  (`-IKnowThisIsUnsafe`) when there is no terminal. The checksum is still
  enforced, but it comes from the same download location, so it proves nothing
  about who built the files.
- Only `https://` is used (`file://` only through the test-only `MDV_BASE_URL`).
- `curl` is preferred; the `fetch`/`wget` fallbacks cannot fully restrict
  redirect targets, which is acceptable because integrity rests on the
  signature and checksum.
- The scripts are tested in CI against local fixtures (tampered archive or
  bundle, wrong identity, missing cosign, wrong binary version, unsafe archive
  paths) in `installers/tests/`; run them locally with
  `sh installers/tests/test-install.sh installers/install.sh`.

### GitHub build provenance

```sh
gh attestation verify md-viewer-0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo daemonhorn/md-viewer
```

## Notes

- Windows binaries are not Authenticode-signed; SmartScreen may warn. The
  cosign signature and attestation are the integrity guarantees.
- All third-party actions are pinned by commit SHA; Dependabot proposes updates.
- Release builds use a pinned Rust toolchain (`RUST_VERSION`) and no
  `rust-cache`, so a poisoned cache cannot reach a release. CI still checks the
  MSRV (1.85) separately.
- `cargo deny` ignores in `deny.toml` carry a reason and a removal condition;
  RUSTSEC-2026-0009 (`time`) can be dropped once the MSRV is raised to 1.88.
