#!/bin/sh
# md-viewer installer for Linux and FreeBSD (x86_64).
#
# Downloads a release from github.com/daemonhorn/md-viewer, verifies its
# SHA-256 checksum and its Sigstore cosign signature (bound to the exact
# release tag), extracts it to a temporary directory, checks that the binary
# reports the requested version and runs `md-viewer install`.
#
# This script never uses sudo and never elevates privileges.
#
# Usage: install.sh [--version X.Y.Z[-pre]] [--pre] [--yes] [--no-assoc]
#                   [--skip-signature-check [--i-know-this-is-unsafe]] [--help]
#
# Latest-version resolution: the newest STABLE release is used. GitHub's
# "latest" never includes pre-releases, so while only pre-releases (e.g.
# 0.1.0-beta1) exist you must pass --version X.Y.Z-pre explicitly, or --pre to
# take the newest release including pre-releases.
#
# Environment (mainly for testing):
#   MDV_BASE_URL      Directory URL (https:// or file://) that directly
#                     contains the release files. Requires --version/MDV_VERSION.
#   MDV_RELEASES_API  URL (https:// or file://) of a GitHub "list releases"
#                     JSON document, used instead of the real API.
#   MDV_VERSION       Same as --version.
set -eu

REPO="daemonhorn/md-viewer"
OIDC_ISSUER='https://token.actions.githubusercontent.com'
VERSION_RE='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'

version="${MDV_VERSION:-}"
skip_sig=0
unsafe_ack=0
allow_pre=0
pass_args=""

die() { printf 'install.sh: error: %s\n' "$*" >&2; exit 1; }
info() { printf '==> %s\n' "$*"; }

usage() {
  cat <<USAGE
Usage: install.sh [options]
  --version X.Y.Z[-pre]    install this version (default: newest stable release)
  --pre                    when resolving the latest version, also consider
                           pre-releases (e.g. 0.1.0-beta1)
  --yes                    pass --yes to 'md-viewer install' (no prompts)
  --no-assoc               pass --no-assoc (do not change file associations)
  --skip-signature-check   do NOT verify the cosign signature (checksum is
                           still verified; this is UNSAFE: it does not prove
                           who built the files). Requires an interactive
                           confirmation, or --i-know-this-is-unsafe.
  --i-know-this-is-unsafe  confirm --skip-signature-check non-interactively
  --help                   show this help

While only pre-releases exist, pass --version or --pre.
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || die "--version needs an argument"
      version="$2"; shift 2 ;;
    --version=*) version="${1#--version=}"; shift ;;
    --pre) allow_pre=1; shift ;;
    --yes|-y) pass_args="$pass_args --yes"; shift ;;
    --no-assoc) pass_args="$pass_args --no-assoc"; shift ;;
    --skip-signature-check) skip_sig=1; shift ;;
    --i-know-this-is-unsafe) unsafe_ack=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; die "unknown option: $1" ;;
  esac
done

valid_version() { printf '%s\n' "$1" | grep -Eq "$VERSION_RE"; }

version="${version#v}"
if [ -n "$version" ]; then
  valid_version "$version" || die "invalid version: $version (expected X.Y.Z or X.Y.Z-pre)"
fi
if [ "$unsafe_ack" -eq 1 ] && [ "$skip_sig" -eq 0 ]; then
  die "--i-know-this-is-unsafe is only meaningful together with --skip-signature-check"
fi

if [ "$(id -u)" = 0 ]; then
  die "refusing to run as root; md-viewer installs per-user. Re-run as a normal user."
fi

# --- skipping signature verification needs explicit consent ----------------
if [ "$skip_sig" -eq 1 ] && [ "$unsafe_ack" -eq 0 ]; then
  printf 'WARNING: --skip-signature-check disables verification of WHO built the files.\n' >&2
  printf 'Only the SHA-256 checksum (from the same download location) will be checked.\n' >&2
  if [ -r /dev/tty ] && ( : </dev/tty ) 2>/dev/null; then
    printf "Type 'yes' to continue without signature verification: " >&2
    answer=""
    read -r answer </dev/tty || answer=""
    [ "$answer" = "yes" ] || die "aborted"
  else
    die "no terminal available for confirmation; pass --i-know-this-is-unsafe to proceed non-interactively"
  fi
fi

# --- platform -------------------------------------------------------------
case "$(uname -s)" in
  Linux)   os_target="unknown-linux-gnu"; is_freebsd=0 ;;
  FreeBSD) os_target="unknown-freebsd"; is_freebsd=1 ;;
  *) die "unsupported OS: $(uname -s) (use install.ps1 on Windows)" ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch="x86_64" ;;
  *) die "unsupported architecture: $(uname -m) (only x86_64 is released)" ;;
esac
target="$arch-$os_target"

# --- tools ----------------------------------------------------------------
# file:// is only reachable through the test-only MDV_* overrides.
allow_file=0
case "${MDV_BASE_URL:-}" in file://*) allow_file=1 ;; esac
case "${MDV_RELEASES_API:-}" in file://*) allow_file=1 ;; esac

check_url() {
  case "$1" in
    https://*) ;;
    file://*) [ "$allow_file" -eq 1 ] || die "refusing non-https URL: $1" ;;
    *) die "refusing non-https URL: $1" ;;
  esac
}

if command -v curl >/dev/null 2>&1; then
  have_curl=1
  if [ "$allow_file" -eq 1 ]; then proto='=https,file'; else proto='=https'; fi
  fetch_to() {
    check_url "$1"
    curl -fsSL --proto "$proto" --proto-redir '=https' --tlsv1.2 \
      --max-filesize 500000000 -o "$2" "$1"
  }
elif command -v fetch >/dev/null 2>&1; then
  # fetch(1) cannot restrict redirect targets; integrity is still enforced by
  # the signature/checksum checks below. Prefer curl when available.
  have_curl=0
  fetch_to() { check_url "$1"; fetch -q -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  have_curl=0
  fetch_to() { check_url "$1"; wget -q --https-only -O "$2" "$1"; }
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

tmp="$(mktemp -d "${TMPDIR:-/tmp}/md-viewer-install.XXXXXX")" || die "mktemp failed"
trap 'rm -rf "$tmp"' EXIT INT TERM HUP

# --- resolve version ------------------------------------------------------
# Newest release from a GitHub "list releases" JSON file. Prints a tag.
# Releases are listed newest first; the document is flattened to one key per
# line so no JSON parser is needed.
newest_tag_from_json() {
  # $1 = file, $2 = 1 to include pre-releases
  # shellcheck disable=SC2020  # three separators, each mapped to a newline
  tr '{},' '\n\n\n' <"$1" | awk -v pre="$2" '
    /"tag_name"[[:space:]]*:/ { t=$0; sub(/.*"tag_name"[[:space:]]*:[[:space:]]*"/,"",t); sub(/".*/,"",t); next }
    /"prerelease"[[:space:]]*:/ {
      if (t != "" && ($0 ~ /false/ || pre == 1)) { print t; exit }
      t=""
    }'
}

if [ -z "$version" ] && { [ -z "${MDV_BASE_URL:-}" ] || [ -n "${MDV_RELEASES_API:-}" ]; }; then
  info "Resolving latest release"
  tag=""
  if [ "$allow_pre" -eq 0 ] && [ "$have_curl" -eq 1 ] && [ -z "${MDV_RELEASES_API:-}" ]; then
    # /releases/latest redirects to the newest stable release (404 if none).
    final="$(curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 \
      -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null)" || final=""
    case "$final" in
      "https://github.com/$REPO/releases/tag/"*) tag="${final##*/}" ;;
    esac
  fi
  if [ -z "$tag" ]; then
    api="${MDV_RELEASES_API:-https://api.github.com/repos/$REPO/releases?per_page=30}"
    fetch_to "$api" "$tmp/releases.json" \
      || die "could not query the GitHub releases API; pass --version X.Y.Z"
    tag="$(newest_tag_from_json "$tmp/releases.json" "$allow_pre")"
  fi
  if [ -z "$tag" ]; then
    if [ "$allow_pre" -eq 1 ]; then
      die "no release found; pass --version X.Y.Z"
    fi
    die "no stable release found (only pre-releases may exist yet). Pass --version X.Y.Z-pre to choose one, or --pre to take the newest pre-release."
  fi
  version="${tag#v}"
  valid_version "$version" || die "latest release tag '$tag' is not a recognised version; pass --version"
fi

if [ -n "${MDV_BASE_URL:-}" ]; then
  [ -n "$version" ] || die "MDV_BASE_URL requires --version (or MDV_VERSION)"
  base="${MDV_BASE_URL%/}"
else
  base="https://github.com/$REPO/releases/download/v$version"
fi

archive="md-viewer-$version-$target.tar.gz"
identity="https://github.com/$REPO/.github/workflows/release.yml@refs/tags/v$version"
info "Installing md-viewer $version ($target)"

cd "$tmp"

info "Downloading $archive, SHA256SUMS and signature bundles"
if ! fetch_to "$base/$archive" "$archive"; then
  if [ "$is_freebsd" -eq 1 ]; then
    cat >&2 <<MSG
install.sh: error: no FreeBSD build could be downloaded for md-viewer $version.
FreeBSD binaries are optional during the beta and may be missing from a
release. Try another --version, or build from source:
  https://github.com/$REPO#building-from-source
MSG
    exit 1
  fi
  die "download failed: $base/$archive"
fi
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
  info "Verifying cosign signatures (identity: $identity)"
  for f in SHA256SUMS "$archive"; do
    if ! cosign verify-blob \
      --bundle "$f.bundle" \
      --certificate-identity "$identity" \
      --certificate-oidc-issuer "$OIDC_ISSUER" \
      "$f" >cosign.out 2>&1; then
      sed 's/^/  cosign: /' cosign.out >&2
      die "cosign signature verification FAILED for $f; aborting"
    fi
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
tar -tzf "$archive" >members.txt || die "cannot read archive"
if grep -Eq '(^/|(^|/)\.\.(/|$))' members.txt; then
  die "archive contains unsafe paths; aborting"
fi
mkdir extract
tar -xzf "$archive" -C extract || die "extraction failed"
bin="extract/md-viewer-$version-$target/md-viewer"
if [ ! -f "$bin" ] || [ -L "$bin" ]; then die "archive does not contain md-viewer-$version-$target/md-viewer"; fi
chmod +x "$bin"

reported="$("$bin" --version 2>/dev/null | awk 'NR==1{print $NF}')" || reported=""
if [ "$reported" != "$version" ]; then
  die "binary reports version '${reported:-<none>}' but $version was requested; aborting"
fi

info "Running md-viewer install"
# shellcheck disable=SC2086  # pass_args is a controlled, space-separated flag list
"$bin" install $pass_args
