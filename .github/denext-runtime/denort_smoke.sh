#!/usr/bin/env bash
# Smoke-test the `denort` archive from assemble_cli.sh with the CLI archive
# beside it: `deno compile` onto the unpacked denort (DENORT_BIN) makes an app
# that has Deno.SQL and names this release in Deno.version.deno; and with no
# DENORT_BIN the CLI asks the release for deno-denort-<version>-<target>.zip
# (DENEXT_DENORT_BASE_URL pointed at a closed loopback port, so the request
# fails and the error names the URL), not Deno's stock denort.
#
# Env: TARGET VERSION ARCHIVE DENORT_ARCHIVE WORK_DIR (a directory that does
# not exist yet)
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
url="http://127.0.0.1:9/denext-runtime-v$VERSION/deno-denort-$VERSION-$TARGET.zip"
test "$rc" != 0 || { echo "::error::deno compile succeeded without a reachable denort" >&2; exit 1; }
case "$err" in
  *"$url"*) echo "deno compile asks for $url" ;;
  *) echo "::error::deno compile did not ask for $url" >&2; exit 1 ;;
esac
