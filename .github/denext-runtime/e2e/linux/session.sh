#!/usr/bin/env bash
# The desktop session the Linux e2e runs in, inside `xvfb-run` +
# `dbus-run-session` (e2e.sh): a window manager (xfwm4: maximize, minimize,
# fullscreen and close requests need one) and a stand-in notification
# server on the private session bus. Then runs "$@".
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)

xfwm4 --compositor=off --sm-client-disable >"${E2E_LOG_DIR:-/tmp}/xfwm4.log" 2>&1 &
wm=$!
export E2E_NOTIFY_LOG=${E2E_NOTIFY_LOG:-${E2E_LOG_DIR:-/tmp}/notify.log}
: >"$E2E_NOTIFY_LOG"
python3 "$here/notification-server.py" >"${E2E_LOG_DIR:-/tmp}/notification-server.log" 2>&1 &
ns=$!
for _ in $(seq 1 50); do
  if wmctrl -m >/dev/null 2>&1 && grep -q ready "${E2E_LOG_DIR:-/tmp}/notification-server.log"; then
    break
  fi
  sleep 0.2
done
echo "window manager: $(wmctrl -m 2>&1 | head -1)"
echo "notification server: $(cat "${E2E_LOG_DIR:-/tmp}/notification-server.log")"

status=0
"$@" || status=$?
kill "$ns" "$wm" 2>/dev/null || true
exit "$status"
