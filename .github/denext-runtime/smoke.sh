#!/usr/bin/env bash
# Package the smoke app with the STOCK `deno desktop` CLI (whatever `deno` is
# on PATH; CI pins 2.9.7) against a runtime archive, the way denext does:
#
#   DENORT_DESKTOP_BIN=<unpacked>/<runtime lib>
#   LAUFEY_DEV_DIR=<unpacked>/laufey
#
# The Node-API probe addon (napi-probe/) is built with the platform C compiler
# (MSVC on PATH on Windows) and embedded in the app; launch.sh checks that it
# loads. SMOKE_NAPI=0 skips it.
#
# Env: TARGET ARCHIVE BACKEND WORK_DIR [SMOKE_NAPI]
# Leaves the packaged app under $WORK_DIR/app/out.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${ARCHIVE:?}" "${BACKEND:?}" "${WORK_DIR:?}"
work=$(unix_path "$WORK_DIR")
archive=$(unix_path "$ARCHIVE")
lib=$(runtime_lib_name "$TARGET")

rm -rf "$work"
mkdir -p "$work"
unpack_archive "$archive" "$work/runtime"
test -f "$work/runtime/$lib"
test -d "$work/runtime/laufey"

cp -R "$here/smoke" "$work/app"
test -f "$work/app/.deno-desktop/app.json"

export DENORT_DESKTOP_BIN
DENORT_DESKTOP_BIN=$(native_path "$work/runtime/$lib")
export LAUFEY_DEV_DIR
LAUFEY_DEV_DIR=$(native_path "$work/runtime/laufey")
echo "DENORT_DESKTOP_BIN=$DENORT_DESKTOP_BIN"
echo "LAUFEY_DEV_DIR=$LAUFEY_DEV_DIR"

include=()
if [ "${SMOKE_NAPI:-1}" != 0 ]; then
  bash "$here/napi-probe/build.sh" "$work/app/probe.node"
  include=(--include probe.node)
fi

cd "$work/app"
deno --version
deno desktop -A --backend "$BACKEND" "${include[@]}" --output out/smoke main.ts

# The Windows CEF layout (e2e/lib/runner.ts windowsCefLayout): laufey's CEF
# executable is CEF's bootstrap.exe, which loads its host laufey.dll as
# <App>.dll; the host loads the runtime as <App>.runtime.dll.
case "$TARGET" in
  *-windows-msvc)
    if [ "$BACKEND" = cef ] && [ -f out/smoke/laufey.dll ]; then
      mv out/smoke/smoke.dll out/smoke/smoke.runtime.dll
      mv out/smoke/laufey.dll out/smoke/smoke.dll
    fi
    ;;
esac

echo "--- packaged output"
find out -maxdepth 4 | sort | head -200
