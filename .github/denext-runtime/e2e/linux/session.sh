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

# E2E_SECRET_SERVICE=locked (e2e.sh): gnome-keyring on this bus with a
# locked login keyring. The keyring is created (unlocked) by a first daemon,
# which then stops; the one the run talks to finds it locked. Both keep
# their files and control socket under E2E_KEYRING_DIR, away from the host's.
keyring=
if [ "${E2E_SECRET_SERVICE:-}" = locked ]; then
  mkdir -p "$E2E_KEYRING_DIR/data" "$E2E_KEYRING_DIR/run"
  chmod 700 "$E2E_KEYRING_DIR/run"
  kr_env=(env XDG_DATA_HOME="$E2E_KEYRING_DIR/data" XDG_RUNTIME_DIR="$E2E_KEYRING_DIR/run")
  printf 'e2e-keyring' | "${kr_env[@]}" gnome-keyring-daemon --foreground --unlock \
    --components=secrets >"${E2E_LOG_DIR:-/tmp}/keyring-create.log" 2>&1 &
  first=$!
  for _ in $(seq 1 50); do
    [ -f "$E2E_KEYRING_DIR/data/keyrings/login.keyring" ] && break
    sleep 0.2
  done
  kill "$first" 2>/dev/null || true
  wait "$first" 2>/dev/null || true
  "${kr_env[@]}" gnome-keyring-daemon --foreground --components=secrets \
    </dev/null >"${E2E_LOG_DIR:-/tmp}/keyring.log" 2>&1 &
  keyring=$!
  locked=
  for _ in $(seq 1 50); do
    locked=$(busctl --user get-property org.freedesktop.secrets \
      /org/freedesktop/secrets/aliases/default \
      org.freedesktop.Secret.Collection Locked 2>/dev/null || true)
    [ -n "$locked" ] && break
    sleep 0.2
  done
  echo "secret service: gnome-keyring, default collection Locked: ${locked:-unknown}"
  [ "$locked" = "b true" ] || { kill "$keyring" "$ns" "$wm" 2>/dev/null; exit 1; }
fi

status=0
"$@" || status=$?
[ -z "$keyring" ] || kill "$keyring" 2>/dev/null || true
kill "$ns" "$wm" 2>/dev/null || true
exit "$status"
