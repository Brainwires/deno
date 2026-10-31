#!/usr/bin/env bash
# Smoke-test the `denort` archive from assemble_denort.sh with the CLI archive
# beside it: `deno compile` onto the unpacked denort (DENORT_BIN) makes an app
# that has Deno.SQL and names this release in Deno.version.deno; and with no
# DENORT_BIN the CLI asks the release for deno-denort-<version>-<target>.zip
# or, first, its SHA256SUMS (DENEXT_DENORT_BASE_URL pointed at a closed
# loopback port, so the request fails and the error names the URL), not Deno's
# stock denort.
#
# With DENORT_PINNED=1 (a CLI built with DENEXT_DENORT_SHA256S, DENEXT.md item
# 187) also: the CLI compiles onto that archive served from a loopback release
# that has no SHA256SUMS (only the hash it was built with can verify it), and
# refuses the same archive with one byte appended.
#
# Env: TARGET VERSION ARCHIVE DENORT_ARCHIVE WORK_DIR (a directory that does
# not exist yet) [DENORT_PINNED]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${VERSION:?}" "${ARCHIVE:?}" "${DENORT_ARCHIVE:?}" "${WORK_DIR:?}"

work=$(unix_path "$WORK_DIR")
test ! -e "$work" || { echo "$work already exists" >&2; exit 1; }
mkdir -p "$work/cli" "$work/rt" "$work/app" "$work/deno-dir"
unpack_to() { # <archive> <dir>
  if command -v unzip >/dev/null 2>&1; then
    unzip -q "$(unix_path "$1")" -d "$2"
  else
    unpack_archive "$(unix_path "$1")" "$2"
  fi
}
unpack_to "$ARCHIVE" "$work/cli"
unpack_to "$DENORT_ARCHIVE" "$work/rt"
bin=$(cli_bin_name "$TARGET")
deno="$work/cli/$bin"
case "$TARGET" in
  *-windows-msvc) rt=denort.exe; app=app.exe ;;
  *) rt=denort; app=app ;;
esac
test -f "$work/rt/$rt" || { echo "::error::no $rt in $DENORT_ARCHIVE" >&2; exit 1; }
test "$(ls -A "$work/rt")" = "$rt" ||
  { echo "::error::$DENORT_ARCHIVE holds more than $rt: $(ls -A "$work/rt" | tr '\n' ' ')" >&2; exit 1; }
case "$TARGET" in
  *-windows-msvc) ;;
  *) test -x "$work/rt/$rt" || { echo "::error::$rt is not executable after unzip" >&2; exit 1; } ;;
esac
export DENO_DIR="$work/deno-dir" DENO_NO_UPDATE_CHECK=1 NO_COLOR=1

cat >"$work/app/main.ts" <<'TS'
console.log(JSON.stringify({ sql: typeof Deno.SQL, version: Deno.version.deno }));
TS

echo "::group::deno compile (DENORT_BIN)"
(cd "$work/app" && DENORT_BIN="$(native_path "$work/rt/$rt")" "$deno" compile --no-config -o app main.ts)
echo "::endgroup::"
out=$("$work/app/$app" | tr -d '\r')
echo "compiled app: $out"
want="{\"sql\":\"function\",\"version\":\"$VERSION\"}"
test "$out" = "$want" || { echo "::error::the compiled app printed '$out', want '$want'" >&2; exit 1; }

echo "::group::deno compile (the release's denort)"
set +e
err=$(cd "$work/app" && DENEXT_DENORT_BASE_URL=http://127.0.0.1:9 "$deno" compile --no-config -o app2 main.ts 2>&1)
rc=$?
set -e
echo "$err"
echo "::endgroup::"
# The CLI fetches the release's SHA256SUMS before the zip (DENEXT.md item
# 130), so the failed request is for either one; both are in the release.
release="http://127.0.0.1:9/denext-runtime-v$VERSION"
url="$release/deno-denort-$VERSION-$TARGET.zip"
sums="$release/SHA256SUMS"
test "$rc" != 0 || { echo "::error::deno compile succeeded without a reachable denort" >&2; exit 1; }
case "$err" in
  *"$url"*) echo "deno compile asks for $url" ;;
  *"$sums"*) echo "deno compile asks for $sums (to verify $url)" ;;
  *) echo "::error::deno compile asked for neither $sums nor $url" >&2; exit 1 ;;
esac

if [ "${DENORT_PINNED:-}" = 1 ]; then
  echo "::group::deno compile (the SHA-256 the CLI was built with)"
  py=python3; command -v python3 >/dev/null 2>&1 || py=python
  asset="deno-denort-$VERSION-$TARGET.zip"
  rel="$work/release/denext-runtime-v$VERSION"
  mkdir -p "$rel"
  cp "$(unix_path "$DENORT_ARCHIVE")" "$rel/$asset"
  "$py" -I - "$(native_path "$work/release")" "$(native_path "$work/port")" <<'PY' &
import functools, http.server, os, sys
root, port_file = sys.argv[1], sys.argv[2]
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=root)
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
with open(port_file + ".tmp", "w") as f:
    f.write(str(server.server_address[1]))
os.replace(port_file + ".tmp", port_file)
server.serve_forever()
PY
  server=$!
  trap 'kill "$server" 2>/dev/null || true' EXIT
  for _ in $(seq 100); do test -s "$work/port" && break; sleep 0.1; done
  test -s "$work/port" || { echo "::error::the loopback release server did not start" >&2; exit 1; }
  base="http://127.0.0.1:$(cat "$work/port")"
  case "$TARGET" in
    *-windows-msvc) app3=app3.exe ;;
    *) app3=app3 ;;
  esac
  (cd "$work/app" && DENO_DIR="$work/deno-dir-pinned" DENEXT_DENORT_BASE_URL="$base" "$deno" compile --no-config -o app3 main.ts)
  out=$("$work/app/$app3" | tr -d '\r')
  test "$out" = "$want" || { echo "::error::the app compiled onto the served denort printed '$out', want '$want'" >&2; exit 1; }
  echo "compiled onto $base/denext-runtime-v$VERSION/$asset with no SHA256SUMS"
  printf 'x' >>"$rel/$asset"
  set +e
  err=$(cd "$work/app" && DENO_DIR="$work/deno-dir-tampered" DENEXT_DENORT_BASE_URL="$base" "$deno" compile --no-config -o app4 main.ts 2>&1)
  rc=$?
  set -e
  echo "$err"
  test "$rc" != 0 || { echo "::error::deno compile took a denort archive whose SHA-256 is not the embedded one" >&2; exit 1; }
  case "$err" in
    *"this CLI was built with"*) echo "a changed $asset is refused" ;;
    *) echo "::error::deno compile failed on the changed archive, but not on its SHA-256" >&2; exit 1 ;;
  esac
  kill "$server" 2>/dev/null || true
  echo "::endgroup::"
fi
