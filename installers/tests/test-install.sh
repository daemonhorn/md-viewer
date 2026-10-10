#!/bin/sh
# Offline tests for installers/install.sh using file:// fixtures and a FAKE
# cosign (it checks the exact arguments install.sh passes and that the bundle
# matches the blob, so the installer's logic is tested, not Sigstore itself).
#
# Optional real-cosign tests run when MDV_TEST_REAL_REL points at a directory
# holding the real v0.1.0-beta1 release files and a real `cosign` is on PATH.
#
# Usage: test-install.sh /path/to/install.sh
set -u

INSTALL="${1:?usage: test-install.sh /path/to/install.sh}"
case "$INSTALL" in /*) ;; *) INSTALL="$PWD/$INSTALL" ;; esac

if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then echo "SKIP: needs Linux x86_64"; exit 0; fi
[ "$(id -u)" != 0 ] || { echo "SKIP: must not run as root"; exit 0; }

work="$(mktemp -d "${TMPDIR:-/tmp}/mdv-inst-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT
pass=0
failed=0
TARGET=x86_64-unknown-linux-gnu
SAFEPATH=/usr/bin:/bin

ok()   { pass=$((pass + 1)); printf 'ok   - %s\n' "$1"; }
nok()  { failed=$((failed + 1)); printf 'FAIL - %s\n' "$1"; [ -z "${2:-}" ] || sed 's/^/       | /' "$2"; }

sha() { sha256sum "$1" | cut -d' ' -f1; }

# Fake cosign: only accepts exact --certificate-identity, never the regexp form.
mkdir "$work/fakebin"
cat >"$work/fakebin/cosign" <<'EOF'
#!/bin/sh
[ "${1:-}" = verify-blob ] || { echo "fake cosign: unexpected subcommand" >&2; exit 2; }
shift
bundle=""; id=""; iss=""
while [ $# -gt 1 ]; do
  case "$1" in
    --bundle) bundle="$2"; shift 2 ;;
    --certificate-identity) id="$2"; shift 2 ;;
    --certificate-oidc-issuer) iss="$2"; shift 2 ;;
    *) echo "fake cosign: unexpected argument $1" >&2; exit 2 ;;
  esac
done
file="$1"
[ "$iss" = "https://token.actions.githubusercontent.com" ] || { echo "bad issuer: $iss" >&2; exit 1; }
[ "$id" = "$FAKE_COSIGN_IDENTITY" ] || { echo "identity mismatch: got $id" >&2; exit 1; }
[ "$(cat "$bundle")" = "SIG $(sha256sum "$file" | cut -d' ' -f1)" ] || { echo "signature mismatch" >&2; exit 1; }
EOF
chmod +x "$work/fakebin/cosign"

# make_release DIR VERSION [REPORTED_VERSION]
make_release() {
  dir="$1"; ver="$2"; rep="${3:-$2}"
  name="md-viewer-$ver-$TARGET"
  mkdir -p "$dir" "$work/stage/$name"
  cat >"$work/stage/$name/md-viewer" <<EOF
#!/bin/sh
case "\$1" in
  --version) echo "md-viewer $rep" ;;
  install) echo "\$*" >"\$HOME/installed.marker" ;;
esac
EOF
  chmod +x "$work/stage/$name/md-viewer"
  tar -czf "$dir/$name.tar.gz" -C "$work/stage" "$name"
  rm -rf "$work/stage"
  sign_all "$dir" "$ver"
}

# (re)generate SHA256SUMS and fake bundles for the files in DIR
sign_all() {
  dir="$1"; ver="$2"
  name="md-viewer-$ver-$TARGET.tar.gz"
  ( cd "$dir" && sha256sum "$name" >SHA256SUMS )
  for f in "$name" SHA256SUMS; do printf 'SIG %s' "$(sha "$dir/$f")" >"$dir/$f.bundle"; done
}

# run_install NAME RELDIR PATH-PREFIX EXPECTED-IDENTITY args...  -> sets $rc, $out
run_install() {
  rdir="$2"; pth="$3"; ident="$4"; shift 4
  home="$(mktemp -d "$work/home.XXXXXX")"
  out="$(mktemp "$work/out.XXXXXX")"
  # setsid drops the controlling terminal so /dev/tty prompts cannot be answered
  if command -v setsid >/dev/null 2>&1; then runner="setsid -w"; else runner=""; fi
  # shellcheck disable=SC2086
  env -i PATH="$pth" HOME="$home" TMPDIR="$work" \
    FAKE_COSIGN_IDENTITY="$ident" MDV_BASE_URL="file://$rdir" \
    $runner sh "$INSTALL" "$@" >"$out" 2>&1 </dev/null
  rc=$?
}

ID() { echo "https://github.com/daemonhorn/md-viewer/.github/workflows/release.yml@refs/tags/v$1"; }
expect_fail() { # name grep-pattern
  if [ "$rc" -ne 0 ] && grep -Eq "$2" "$out" && [ ! -e "$home/installed.marker" ]; then ok "$1"; else nok "$1 (rc=$rc)" "$out"; fi
}

# --- 1. happy path --------------------------------------------------------
make_release "$work/r1" 1.2.3
run_install t "$work/r1" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version 1.2.3 --yes --no-assoc
if [ "$rc" -eq 0 ] && grep -q -- '--yes' "$home/installed.marker" && grep -q -- '--no-assoc' "$home/installed.marker"; then
  ok "valid release installs and passes flags"
else nok "valid release installs (rc=$rc)" "$out"; fi

run_install t "$work/r1" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version v1.2.3
if [ "$rc" -eq 0 ]; then ok "leading v accepted"; else nok "leading v accepted" "$out"; fi

# --- 2. tampering ---------------------------------------------------------
make_release "$work/r2" 1.2.3
printf 'evil' >>"$work/r2/md-viewer-1.2.3-$TARGET.tar.gz"
run_install t "$work/r2" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version 1.2.3
expect_fail "tampered archive rejected" 'FAILED'

make_release "$work/r3" 1.2.3
printf 'SIG deadbeef' >"$work/r3/md-viewer-1.2.3-$TARGET.tar.gz.bundle"
run_install t "$work/r3" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version 1.2.3
expect_fail "tampered archive bundle rejected" 'FAILED'

make_release "$work/r4" 1.2.3
printf 'SIG deadbeef' >"$work/r4/SHA256SUMS.bundle"
run_install t "$work/r4" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version 1.2.3
expect_fail "tampered SHA256SUMS bundle rejected" 'FAILED'

# validly signed but checksum list disagrees with archive
make_release "$work/r5" 1.2.3
printf '%s  md-viewer-1.2.3-%s.tar.gz\n' "$(printf x | sha256sum | cut -d' ' -f1)" "$TARGET" >"$work/r5/SHA256SUMS"
printf 'SIG %s' "$(sha "$work/r5/SHA256SUMS")" >"$work/r5/SHA256SUMS.bundle"
run_install t "$work/r5" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version 1.2.3
expect_fail "checksum mismatch rejected" 'MISMATCH'

# --- 3. identity ----------------------------------------------------------
run_install t "$work/r1" "$work/fakebin:$SAFEPATH" "$(ID 9.9.9)" --version 1.2.3
expect_fail "wrong certificate identity rejected" 'FAILED'
run_install t "$work/r1" "$work/fakebin:$SAFEPATH" "https://github.com/evil/md-viewer/.github/workflows/release.yml@refs/tags/v1.2.3" --version 1.2.3
expect_fail "wrong repo identity rejected" 'FAILED'

# --- 4. missing cosign ----------------------------------------------------
run_install t "$work/r1" "$SAFEPATH" "$(ID 1.2.3)" --version 1.2.3
expect_fail "missing cosign refused" "'cosign' was not found"

# --- 5. binary version binding --------------------------------------------
make_release "$work/r6" 1.2.3 1.2.2
run_install t "$work/r6" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version 1.2.3
expect_fail "binary reporting a different version rejected" 'reports version'

# --- 6. unsafe archive paths ----------------------------------------------
mkdir -p "$work/r7" "$work/evil/x"
echo hi >"$work/evil/x/y"
tar -czf "$work/r7/md-viewer-1.2.3-$TARGET.tar.gz" -C "$work/evil" --transform 's,^x/,../x/,' x 2>/dev/null
sign_all "$work/r7" 1.2.3
run_install t "$work/r7" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version 1.2.3
expect_fail "archive with .. paths rejected" 'unsafe paths'

# --- 7. input validation --------------------------------------------------
for bad in '1.2' '1.2.3;rm' '../1.2.3' '1.2.3+x' '1.2.3-' ''; do
  [ -n "$bad" ] || continue
  run_install t "$work/r1" "$work/fakebin:$SAFEPATH" "$(ID 1.2.3)" --version "$bad"
  expect_fail "invalid version '$bad' rejected" 'invalid version'
done

# --- 8. --skip-signature-check consent ------------------------------------
run_install t "$work/r1" "$SAFEPATH" "" --version 1.2.3 --skip-signature-check
expect_fail "--skip-signature-check without consent refused (no tty)" 'i-know-this-is-unsafe'
run_install t "$work/r1" "$SAFEPATH" "" --version 1.2.3 --skip-signature-check --i-know-this-is-unsafe --yes
if [ "$rc" -eq 0 ] && grep -q 'WARNING' "$out"; then ok "--skip-signature-check with --i-know-this-is-unsafe proceeds (with warning)"; else nok "skip with ack" "$out"; fi
run_install t "$work/r1" "$SAFEPATH" "" --version 1.2.3 --i-know-this-is-unsafe
expect_fail "--i-know-this-is-unsafe alone rejected" 'only meaningful'
# skipping the signature must still enforce the checksum
run_install t "$work/r5" "$SAFEPATH" "" --version 1.2.3 --skip-signature-check --i-know-this-is-unsafe
expect_fail "checksum still enforced when signature check skipped" 'MISMATCH'

# --- 9. non-https URLs refused --------------------------------------------
home="$work/h9"; mkdir -p "$home"
env -i PATH="$work/fakebin:$SAFEPATH" HOME="$home" TMPDIR="$work" FAKE_COSIGN_IDENTITY="$(ID 1.2.3)" \
  MDV_BASE_URL="http://127.0.0.1:9/x" sh "$INSTALL" --version 1.2.3 >"$work/o9" 2>&1 </dev/null
rc=$?
if [ "$rc" -ne 0 ] && grep -q 'refusing non-https' "$work/o9"; then ok "http:// base URL refused"; else out="$work/o9"; nok "http base refused" "$work/o9"; fi

# --- 10. latest-version resolution (offline API fixture) ------------------
cat >"$work/api-mixed.json" <<'EOF'
[ {"tag_name": "v0.3.0-rc1", "prerelease": true, "assets": []},
  {"tag_name": "v0.2.0", "prerelease": false, "assets": []},
  {"tag_name": "v0.1.0", "prerelease": false, "assets": []} ]
EOF
cat >"$work/api-pre-only.json" <<'EOF'
[{"id":1,"tag_name":"v0.1.0-beta1","draft":false,"prerelease":true,"author":{"login":"x"}}]
EOF
mkdir -p "$work/api-empty"; echo '[]' >"$work/api-empty.json"
api_run() { # json-file expected-version args...
  apifile="$1"; ver="$2"; shift 2
  make_release "$work/rr-$ver" "$ver"
  home="$(mktemp -d "$work/h10.XXXXXX")"; out="$(mktemp "$work/o10.XXXXXX")"
  env -i PATH="$work/fakebin:$SAFEPATH" HOME="$home" TMPDIR="$work" FAKE_COSIGN_IDENTITY="$(ID "$ver")" \
    MDV_BASE_URL="file://$work/rr-$ver" MDV_RELEASES_API="file://$apifile" sh "$INSTALL" "$@" >"$out" 2>&1 </dev/null
  rc=$?
}
api_run "$work/api-mixed.json" 0.2.0
if [ "$rc" -eq 0 ] && grep -q 'md-viewer 0.2.0 ' "$out"; then ok "latest = newest stable (skips pre-release)"; else nok "latest stable" "$out"; fi
api_run "$work/api-mixed.json" 0.3.0-rc1 --pre
if [ "$rc" -eq 0 ] && grep -q 'md-viewer 0.3.0-rc1 ' "$out"; then ok "--pre = newest including pre-release"; else nok "latest --pre" "$out"; fi
api_run "$work/api-pre-only.json" 0.1.0-beta1
if [ "$rc" -ne 0 ] && grep -q 'Pass --version' "$out"; then ok "only pre-releases: tells user to pass --version"; else nok "pre-only error" "$out"; fi
api_run "$work/api-pre-only.json" 0.1.0-beta1 --pre
if [ "$rc" -eq 0 ]; then ok "only pre-releases + --pre installs it"; else nok "pre-only --pre" "$out"; fi
printf '[{"tag_name": "v1.0.0;evil", "prerelease": false}]' >"$work/api-bad.json"
api_run "$work/api-bad.json" 1.0.0
if [ "$rc" -ne 0 ] && grep -q 'not a recognised version' "$out"; then ok "malformed resolved tag rejected"; else nok "bad tag" "$out"; fi

# --- 11. real cosign + real beta1 release (optional) -----------------------
REAL="${MDV_TEST_REAL_REL:-}"
if [ -n "$REAL" ] && [ -f "$REAL/SHA256SUMS" ] && command -v cosign >/dev/null 2>&1; then
  cdir="$(dirname "$(command -v cosign)")"
  RV=0.1.0-beta1
  RT="md-viewer-$RV-$TARGET.tar.gz"
  # tampered archive
  mkdir "$work/real1"; cp "$REAL/$RT" "$REAL/$RT.bundle" "$REAL/SHA256SUMS" "$REAL/SHA256SUMS.bundle" "$work/real1/"
  printf 'x' >>"$work/real1/$RT"
  run_install t "$work/real1" "$cdir:$SAFEPATH" "" --version "$RV"
  expect_fail "[real cosign] tampered archive rejected" 'FAILED'
  # genuine bytes presented as a different version: identity is bound to the tag
  mkdir "$work/real2"
  for f in "$RT" "$RT.bundle"; do cp "$REAL/$f" "$work/real2/$(echo "$f" | sed 's/0.1.0-beta1/0.1.0-beta2/')"; done
  sed 's/0.1.0-beta1/0.1.0-beta2/g' "$REAL/SHA256SUMS" >"$work/real2/SHA256SUMS"
  cp "$REAL/SHA256SUMS.bundle" "$work/real2/"
  run_install t "$work/real2" "$cdir:$SAFEPATH" "" --version 0.1.0-beta2
  expect_fail "[real cosign] genuine files replayed as another version rejected" 'FAILED'
  # genuine release: must get past signature + checksum (binary may still need GUI libs)
  run_install t "$REAL" "$cdir:$SAFEPATH" "" --version "$RV" --yes --no-assoc
  if grep -q 'Verifying SHA-256 checksum' "$out" && grep -q 'Extracting' "$out"; then ok "[real cosign] genuine beta1 passes signature and checksum"; else nok "real genuine" "$out"; fi
else
  echo "skip - real cosign tests (set MDV_TEST_REAL_REL and put cosign on PATH)"
fi

printf '\n%s passed, %s failed\n' "$pass" "$failed"
[ "$failed" -eq 0 ]
