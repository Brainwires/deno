#!/usr/bin/env bash
# Package the smoke app with the STOCK `deno desktop` CLI (whatever `deno` is
# on PATH; CI pins 2.9.7) against a runtime archive, the way denext does:
#
#   DENORT_DESKTOP_BIN=<unpacked>/<runtime lib>
#   LAUFEY_DEV_DIR=<unpacked>/laufey
#
# Env: TARGET ARCHIVE BACKEND WORK_DIR
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

cd "$work/app"
deno --version
deno desktop -A --backend "$BACKEND" --output out/smoke main.ts

echo "--- packaged output"
find out -maxdepth 4 | sort | head -200
