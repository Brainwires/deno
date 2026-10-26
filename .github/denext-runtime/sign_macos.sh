#!/usr/bin/env bash
# Developer ID-sign Mach-O files in place, then notarize them (P1): the
# release's CLI (`deno`), `denort` and `libdenort.dylib`. Run BEFORE the
# archives (and their SHA-256 pins) are made, so the pins cover the signed
# bytes.
#
#   sign_macos.sh [--entitlements <plist>] <file>...
#
# Executables get the hardened runtime with the given entitlements (the CLI
# and denort take cli/entitlements.plist, upstream Deno's set: V8's JIT,
# Node-API / FFI libraries signed by others, ...); a dylib is signed without.
# A bare Mach-O cannot be stapled, so Gatekeeper checks the notarization
# ticket online on first run.
#
# Env (repository secrets; none set = skipped with a warning, the files keep
# their ad-hoc signature):
#   APPLE_DEVELOPER_ID_CERT           base64 of the Developer ID Application .p12
#   APPLE_DEVELOPER_ID_CERT_PASSWORD  its password
#   APPLE_API_KEY                     base64 of an App Store Connect API key (.p8)
#   APPLE_API_KEY_ID, APPLE_API_ISSUER  that key's id and issuer
#   APPLE_TEAM_ID                     the Team ID the signature must carry
#                                     (WT957SC7MJ; the workflow sets it)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

entitlements=
if [ "${1:-}" = --entitlements ]; then
  entitlements=$2
  shift 2
  need "the entitlements file $entitlements" test -f "$entitlements"
fi
[ $# -gt 0 ] || { echo "::error::sign_macos.sh: no files to sign" >&2; exit 1; }
for f in "$@"; do need "the file to sign, $f" test -f "$f"; done

if [ -z "${APPLE_DEVELOPER_ID_CERT:-}" ]; then
  echo "::warning::no APPLE_DEVELOPER_ID_CERT secret: $* keep an ad-hoc signature (not Developer ID-signed or notarized; Gatekeeper refuses a quarantined copy)"
  exit 0
fi
: "${APPLE_DEVELOPER_ID_CERT_PASSWORD:?the APPLE_DEVELOPER_ID_CERT_PASSWORD secret is missing}"
: "${APPLE_TEAM_ID:?}"

umask 077
tmp=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sign.XXXXXX")
kc="$tmp/sign.keychain-db"
cleanup() {
  security delete-keychain "$kc" >/dev/null 2>&1 || true
  rm -rf "$tmp"
}
trap cleanup EXIT

# A throwaway keychain holding only the Developer ID identity.
printf '%s' "$APPLE_DEVELOPER_ID_CERT" | base64 --decode >"$tmp/cert.p12"
kc_pw=$(openssl rand -hex 24)
security create-keychain -p "$kc_pw" "$kc"
security set-keychain-settings -lut 21600 "$kc"
security unlock-keychain -p "$kc_pw" "$kc"
security import "$tmp/cert.p12" -k "$kc" -f pkcs12 -P "$APPLE_DEVELOPER_ID_CERT_PASSWORD" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kc_pw" "$kc" >/dev/null
# On the search list too, so codesign finds the identity's chain.
existing=()
while IFS= read -r line; do
  line=${line#"${line%%[![:space:]]*}"}
  line=${line#\"}
  line=${line%\"}
  [ -n "$line" ] && existing+=("$line")
done < <(security list-keychains -d user)
security list-keychains -d user -s "$kc" "${existing[@]}"

identity=$(security find-identity -v -p codesigning "$kc" |
  awk -v team="($APPLE_TEAM_ID)" '/"Developer ID Application: / && index($0, team) { print $2; exit }')
if [ -z "$identity" ]; then
  echo "::error::APPLE_DEVELOPER_ID_CERT holds no valid 'Developer ID Application: ... ($APPLE_TEAM_ID)' identity"
  exit 1
fi

requirement="anchor apple generic and certificate leaf[subject.OU] = \"$APPLE_TEAM_ID\""
for f in "$@"; do
  args=(--force --timestamp --keychain "$kc" --sign "$identity")
  if file -b "$f" | grep -q 'executable'; then
    args+=(--options runtime)
    [ -z "$entitlements" ] || args+=(--entitlements "$entitlements")
  fi
  codesign "${args[@]}" "$f"
  codesign --verify --strict --verbose=2 -R="$requirement" "$f" ||
    { echo "::error::$f does not verify as signed by Team ID $APPLE_TEAM_ID after signing"; exit 1; }
  codesign -dv "$f" 2>&1 | grep -E '^(Authority|TeamIdentifier|Timestamp|flags)' | sed "s|^|$(basename "$f"): |"
done

if [ -z "${APPLE_API_KEY:-}" ] || [ -z "${APPLE_API_KEY_ID:-}" ] || [ -z "${APPLE_API_ISSUER:-}" ]; then
  echo "::warning::no APPLE_API_KEY / APPLE_API_KEY_ID / APPLE_API_ISSUER secrets: $* are Developer ID-signed but NOT notarized"
  exit 0
fi
printf '%s' "$APPLE_API_KEY" | base64 --decode >"$tmp/key.p8"
zip -q -j "$tmp/notarize.zip" "$@"
xcrun notarytool submit "$tmp/notarize.zip" \
  --key "$tmp/key.p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" \
  --wait --timeout 45m --output-format json >"$tmp/notary.json" || true
status=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("status", ""))' "$tmp/notary.json" 2>/dev/null || true)
id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id", ""))' "$tmp/notary.json" 2>/dev/null || true)
echo "notarization ${id:-<no id>}: ${status:-<no status>}"
if [ "$status" != Accepted ]; then
  cat "$tmp/notary.json" || true
  if [ -n "$id" ]; then
    xcrun notarytool log "$id" --key "$tmp/key.p8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" || true
  fi
  echo "::error::notarization of $* was not accepted (status '${status:-none}')"
  exit 1
fi
