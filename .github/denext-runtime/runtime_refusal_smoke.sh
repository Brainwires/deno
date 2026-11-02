#!/usr/bin/env bash
# Copyright 2018-2026 the Deno authors. MIT license.
# A denext CLI builds no app on an upstream runtime it cannot run (DENEXT.md
# item 216): `deno compile --engine quickjs` and `deno desktop` (with and
# without it), given no DENORT_BIN / DENORT_DESKTOP_BIN, each exit non-zero
# naming the archive and the fix, write no app, cache nothing and open no
# connection (a loopback proxy records any).
#
# Usage: runtime_refusal_smoke.sh <deno> <work dir that does not exist yet>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

deno=$1
work=$(unix_path "$2")
test ! -e "$work" || { echo "$work already exists" >&2; exit 1; }
mkdir -p "$work/app" "$work/laufey"
# DENO_CHECK_ENGINE: a CLI with no tsc beside it still reaches the base binary.
export DENO_DIR="$work/deno-dir" DENO_NO_UPDATE_CHECK=1 NO_COLOR=1 DENO_CHECK_ENGINE=tsc
# A build under target/ would take the runtimes beside it instead.
case "$deno" in
  */target/*) echo "::error::copy $deno out of target/ first" >&2; exit 1 ;;
esac
unset DENORT_BIN DENORT_DESKTOP_BIN
echo 'console.log("ran");' >"$work/app/main.ts"

py=python3; command -v python3 >/dev/null 2>&1 || py=python
"$py" -I - "$(native_path "$work/port")" "$(native_path "$work/connections")" <<'PY' &
import os, socket, sys
port_file, seen = sys.argv[1], sys.argv[2]
server = socket.create_server(("127.0.0.1", 0))
with open(port_file + ".tmp", "w") as f:
    f.write(str(server.getsockname()[1]))
os.replace(port_file + ".tmp", port_file)
while True:
    conn, _ = server.accept()
    with open(seen, "a") as f:
        f.write("connection\n")
    conn.close()
PY
proxy=$!
trap 'kill "$proxy" 2>/dev/null || true' EXIT
wait_for_file "$work/port" "$proxy" || { echo "::error::the loopback proxy did not start" >&2; exit 1; }
url="http://127.0.0.1:$(cat "$work/port")"
export HTTP_PROXY="$url" HTTPS_PROXY="$url" http_proxy="$url" https_proxy="$url"
unset NO_PROXY no_proxy

expect_refused() { # <asset prefix> <fix> <output> <args...>
  local asset=$1 fix=$2 output=$3 err rc
  shift 3
  set +e
  err=$(cd "$work/app" && "$deno" "$@" -o "$output" main.ts 2>&1)
  rc=$?
  set -e
  echo "$err"
  test "$rc" != 0 || { echo "::error::$* built an app on $asset" >&2; exit 1; }
  case "$err" in
    *"$asset-"*"Nothing was downloaded"*"$fix"*) echo "refused: $*" ;;
    *) echo "::error::$* failed, but not with the refusal naming $asset and $fix" >&2; exit 1 ;;
  esac
  test -z "$(ls "$work/app" | grep -v '^main\.ts$' || true)" ||
    { echo "::error::$* left output: $(ls "$work/app")" >&2; exit 1; }
  test ! -e "$DENO_DIR/dl" || { echo "::error::$* cached a download" >&2; exit 1; }
  test ! -e "$work/connections" || { echo "::error::$* opened a connection" >&2; exit 1; }
}

expect_refused denort-quickjs DENORT_BIN app1 compile --no-config --engine quickjs
# LAUFEY_DEV_DIR: the base binary is the only thing left to refuse.
export LAUFEY_DEV_DIR="$(native_path "$work/laufey")"
expect_refused libdenort DENORT_DESKTOP_BIN app2 desktop --no-config --backend webview
expect_refused libdenort-quickjs DENORT_DESKTOP_BIN app3 desktop --no-config --backend webview --engine quickjs
echo "every upstream runtime is refused"
