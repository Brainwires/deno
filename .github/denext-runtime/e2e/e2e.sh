#!/usr/bin/env bash
# Run the denext runtime e2e suite (run.ts) against one runtime archive and
# backend, with the STOCK `deno` on PATH as the packaging CLI (CI pins
# 2.9.7). On Linux it runs inside Xvfb + a private D-Bus session with a
# window manager and a stand-in notification server (linux/session.sh).
#
# Env: TARGET ARCHIVE BACKEND WORK_DIR [E2E_OUT] [E2E_AREAS=a,b,...]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/../lib.sh"

: "${TARGET:?}" "${ARCHIVE:?}" "${BACKEND:?}" "${WORK_DIR:?}"
work=$(unix_path "$WORK_DIR")
archive=$(unix_path "$ARCHIVE")
rm -rf "$work"
mkdir -p "$work/w/logs"
unpack_archive "$archive" "$work/runtime"
test -f "$work/runtime/$(runtime_lib_name "$TARGET")"

export RUNTIME_DIR WORK_DIR E2E_OUT
RUNTIME_DIR=$(native_path "$work/runtime")
WORK_DIR=$(native_path "$work/w")
E2E_OUT=$(native_path "${E2E_OUT:-$work/out}")
mkdir -p "$(unix_path "$E2E_OUT")"
deno --version | head -1

run=(deno run -A --no-config --no-lock "$here/run.ts")
case "$TARGET" in
  *-linux-gnu)
    export WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
    export E2E_LOG_DIR="$work/w/logs"
    # No desktop environment: xdg-utils' generic mode, as on a bare session.
    unset XDG_CURRENT_DESKTOP DESKTOP_SESSION
    xvfb-run --auto-servernum --server-args="-screen 0 1600x1000x24" \
      dbus-run-session -- bash "$here/linux/session.sh" "${run[@]}"
    ;;
  *)
    "${run[@]}"
    ;;
esac
