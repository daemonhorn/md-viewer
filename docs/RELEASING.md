# Releasing md-viewer

## Cutting a release

1. Make sure `main` is green in CI (including `cargo deny`).
2. Bump `version` in the workspace `Cargo.toml` (and update `Cargo.lock`).
   The release workflow fails if the tag does not match this version.
3. Commit, push to `main`, then tag and push:

   ```sh
   git tag -s v0.1.0 -m "md-viewer 0.1.0"   # or: git tag v0.1.0
   git push origin v0.1.0
   ```

   Tags containing a `-` (e.g. `v0.1.0-rc1`) are published as pre-releases.
4. The `Release` workflow then:
   - builds Windows (MSVC), Linux (ubuntu-22.04, glibc) and FreeBSD 14 binaries,
   - packages `md-viewer-<ver>-x86_64-pc-windows-msvc.zip`,
     `md-viewer-<ver>-x86_64-unknown-linux-gnu.tar.gz`,
     `md-viewer-<ver>-x86_64-unknown-freebsd.tar.gz`,
   - generates `SHA256SUMS` and a CycloneDX SBOM (`md-viewer-<ver>.cdx.json`),
   - signs every file and `SHA256SUMS` with cosign keyless (`<file>.bundle`),
   - creates a GitHub build-provenance attestation,
   - creates the GitHub release (the only job with `contents: write`).
5. Verify the published release using the commands below, then test the
   installers (`installers/install.sh`, `installers/install.ps1`).

If a release build fails, delete the tag (`git push --delete origin vX.Y.Z`),
fix, and re-tag. Never reuse a version that was successfully published.

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
  --certificate-identity-regexp '^https://github.com/daemonhorn/md-viewer/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  md-viewer-0.1.0-x86_64-unknown-linux-gnu.tar.gz
```

Do the same for `SHA256SUMS` / `SHA256SUMS.bundle`.

### GitHub build provenance

```sh
gh attestation verify md-viewer-0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo daemonhorn/md-viewer
```

## Notes

- Windows binaries are not Authenticode-signed; SmartScreen may warn. The
  cosign signature and attestation are the integrity guarantees.
- All third-party actions are pinned by commit SHA; Dependabot proposes updates.
