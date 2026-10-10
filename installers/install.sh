#!/bin/sh
# md-viewer installer for Linux and FreeBSD (x86_64).
#
# Downloads a release from github.com/daemonhorn/md-viewer, verifies its
# SHA-256 checksum and its Sigstore cosign signature, extracts it to a
# temporary directory and runs `md-viewer install`.
#
# This script never uses sudo and never elevates privileges.
#
# Usage: install.sh [--version X.Y.Z] [--yes] [--no-assoc]
#                   [--skip-signature-check] [--help]
#
# Environment (mainly for testing):
#   MDV_BASE_URL   Directory URL (https:// or file://) that directly contains
#                  the release files. Requires --version or MDV_VERSION.
#   MDV_VERSION    Same as --version.
set -eu

REPO="daemonhorn/md-viewer"
IDENTITY_RE='^https://github.com/daemonhorn/md-viewer/\.github/workflows/release\.yml@refs/tags/v'
OIDC_ISSUER='https://token.actions.githubusercontent.com'

version="${MDV_VERSION:-}"
skip_sig=0
pass_args=""

die() { printf 'install.sh: error: %s\n' "$*" >&2; exit 1; }
info() { printf '==> %s\n' "$*"; }

usage() {
  cat <<USAGE
Usage: install.sh [options]
  --version X.Y.Z          install this version (default: latest release)
  --yes                    pass --yes to 'md-viewer install' (no prompts)
  --no-assoc               pass --no-assoc (do not change file associations)
  --skip-signature-check   do NOT verify the cosign signature (checksum is
                           still verified; this is less safe)
  --help                   show this help
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || die "--version needs an argument"
      version="$2"; shift 2 ;;
    --version=*) version="${1#--version=}"; shift ;;
    --yes|-y) pass_args="$pass_args --yes"; shift ;;
    --no-assoc) pass_args="$pass_args --no-assoc"; shift ;;
    --skip-signature-check) skip_sig=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

version="${version#v}"
case "$version" in
  *[!0-9A-Za-z.+-]*) die "invalid version: $version" ;;
esac

if [ "$(id -u)" = 0 ]; then
  die "refusing to run as root; md-viewer installs per-user. Re-run as a normal user."
fi

# --- platform -------------------------------------------------------------
case "$(uname -s)" in
  Linux)   os_target="unknown-linux-gnu" ;;
  FreeBSD) os_target="unknown-freebsd" ;;
  *) die "unsupported OS: $(uname -s) (use install.ps1 on Windows)" ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch="x86_64" ;;
  *) die "unsupported architecture: $(uname -m) (only x86_64 is released)" ;;
esac
target="$arch-$os_target"

# --- tools ----------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
  fetch_to() { curl -fsSL --proto '=https,file' -o "$2" "$1"; }
elif command -v fetch >/dev/null 2>&1; then
  fetch_to() { fetch -q -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch_to() { wget -q -O "$2" "$1"; }
else
  die "need curl, fetch or wget"
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{print $1}'; }
elif command -v sha256 >/dev/null 2>&1; then
  sha256_of() { sha256 -q "$1"; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  die "need sha256sum, sha256 or shasum"
fi

# --- resolve version / URLs -----------------------------------------------
if [ -n "${MDV_BASE_URL:-}" ]; then
  [ -n "$version" ] || die "MDV_BASE_URL requires --version (or MDV_VERSION)"
  base="${MDV_BASE_URL%/}"
else
  if [ -z "$version" ]; then
    info "Resolving latest release"
    command -v curl >/dev/null 2>&1 || die "curl is required to resolve the latest version; pass --version instead"
    final="$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" \
      || die "could not resolve latest release"
    tag="${final##*/}"
    version="${tag#v}"
    case "$version" in ""|latest) die "could not determine latest version" ;; esac
  fi
  base="https://github.com/$REPO/releases/download/v$version"
fi

archive="md-viewer-$version-$target.tar.gz"
info "Installing md-viewer $version ($target)"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/md-viewer-install.XXXXXX")" || die "mktemp failed"
trap 'rm -rf "$tmp"' EXIT INT TERM HUP
cd "$tmp"

info "Downloading $archive, SHA256SUMS and signature bundles"
fetch_to "$base/$archive" "$archive" || die "download failed: $base/$archive"
fetch_to "$base/SHA256SUMS" SHA256SUMS || die "download failed: $base/SHA256SUMS"
if [ "$skip_sig" -eq 0 ]; then
  fetch_to "$base/$archive.bundle" "$archive.bundle" || die "download failed: $archive.bundle"
  fetch_to "$base/SHA256SUMS.bundle" SHA256SUMS.bundle || die "download failed: SHA256SUMS.bundle"
fi

# --- verify signature -----------------------------------------------------
if [ "$skip_sig" -eq 1 ]; then
  printf 'WARNING: skipping cosign signature verification (--skip-signature-check).\n' >&2
else
  if ! command -v cosign >/dev/null 2>&1; then
    cat >&2 <<MSG
install.sh: error: 'cosign' was not found, so the release signature cannot be verified.

Install cosign (https://docs.sigstore.dev/cosign/system_config/installation/):
  Linux:    download from https://github.com/sigstore/cosign/releases, or
            use your package manager (e.g. 'brew install cosign')
  FreeBSD:  pkg install cosign

Alternatives:
  * Verify with the GitHub CLI after downloading the archive:
      gh attestation verify $archive --repo $REPO
  * Re-run this script with --skip-signature-check to rely on the SHA-256
    checksum only (NOT recommended: it does not prove who built the files).
MSG
    exit 1
  fi
  info "Verifying cosign signatures"
  for f in SHA256SUMS "$archive"; do
    cosign verify-blob \
      --bundle "$f.bundle" \
      --certificate-identity-regexp "$IDENTITY_RE" \
      --certificate-oidc-issuer "$OIDC_ISSUER" \
      "$f" >/dev/null 2>&1 \
      || die "cosign signature verification FAILED for $f; aborting"
  done
fi

# --- verify checksum ------------------------------------------------------
info "Verifying SHA-256 checksum"
expected="$(awk -v f="$archive" '{n=$2; sub(/^\*/,"",n); if (n==f) {print $1; exit}}' SHA256SUMS)"
[ -n "$expected" ] || die "$archive not listed in SHA256SUMS"
actual="$(sha256_of "$archive")"
if [ "$expected" != "$actual" ]; then
  die "checksum MISMATCH for $archive (expected $expected, got $actual); aborting"
fi

# --- extract and install --------------------------------------------------
info "Extracting"
mkdir extract
tar -xzf "$archive" -C extract || die "extraction failed"
bin="extract/md-viewer-$version-$target/md-viewer"
[ -f "$bin" ] || die "archive does not contain md-viewer-$version-$target/md-viewer"
chmod +x "$bin"

info "Running md-viewer install"
# shellcheck disable=SC2086  # pass_args is a controlled, space-separated flag list
"$bin" install $pass_args
