#!/usr/bin/env bash
# Run the denext runtime e2e suite (run.ts) against one runtime archive and
# backend, with the STOCK `deno` on PATH as the packaging CLI (CI pins
# 2.9.7). On Linux it runs inside Xvfb + a private D-Bus session with a
# window manager and a stand-in notification server (linux/session.sh).
#
# Env: TARGET ARCHIVE BACKEND WORK_DIR [E2E_OUT] [E2E_AREAS=a,b,...]
#      [E2E_SECRET_SERVICE=masked|locked] (Linux; masked by default)
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
    # The secret service. Masked (the default): none in the session, as on
    # the GitHub runner, so a host's gnome-keyring can't hold the run up
    # (gcr-prompter would also grab the pointer and keyboard from the input
    # checks). Locked (E2E_SECRET_SERVICE=locked): a real gnome-keyring on
    # the session bus whose login keyring is locked, with gcr-prompter
    # installed and no one to answer it, the case that used to hold every
    # request CEF sends with cookies (navigations, WebSocket handshakes):
    # the runtime must fall back to --password-store=basic (laufey API 45),
    # and the keyring area checks that nothing stalls (linux/session.sh
    # starts the keyring).
    export E2E_SECRET_SERVICE="${E2E_SECRET_SERVICE:-masked}"
    case "$E2E_SECRET_SERVICE" in
      masked)
        mkdir -p "$work/xdg/dbus-1/services"
        for svc in org.freedesktop.secrets org.freedesktop.impl.portal.Secret org.gnome.keyring; do
          printf '[D-BUS Service]\nName=%s\nExec=/bin/false\n' "$svc" >"$work/xdg/dbus-1/services/$svc.service"
        done
        export XDG_DATA_DIRS="$work/xdg:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
        ;;
      locked)
        command -v gnome-keyring-daemon >/dev/null || {
          echo "E2E_SECRET_SERVICE=locked needs gnome-keyring-daemon" >&2
          exit 1
        }
        export E2E_KEYRING_DIR="$work/keyring"
        ;;
      *)
        echo "E2E_SECRET_SERVICE: masked or locked, not $E2E_SECRET_SERVICE" >&2
        exit 1
        ;;
    esac
    xvfb-run --auto-servernum --server-args="-screen 0 1600x1000x24" \
      dbus-run-session -- bash "$here/linux/session.sh" "${run[@]}"
    ;;
  *)
    "${run[@]}"
    ;;
esac
