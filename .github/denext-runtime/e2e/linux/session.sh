#!/usr/bin/env bash
# The desktop session the Linux e2e runs in, inside `dbus-run-session`
# (e2e.sh): a window manager and a stand-in notification server on the private
# session bus. Then runs "$@".
#
# E2E_DISPLAY=x11 (the default, inside `xvfb-run`): xfwm4, since maximize,
# minimize, fullscreen and close requests need a window manager.
# E2E_DISPLAY=wayland: sway on wlroots' headless backend, nothing else. It has
# no X server: XWayland is off and $DISPLAY unset, so a window can only be a
# native Wayland one (see linux/sway.conf for the rest).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)

if [ "${E2E_DISPLAY:-x11}" = wayland ]; then
  # The socket and sway's IPC socket live here. A short path: a Unix socket's
  # is limited to 107 bytes, and a runner's temp directory is deep.
  XDG_RUNTIME_DIR=$(mktemp -d /tmp/e2e-wl.XXXXXX)
  chmod 700 "$XDG_RUNTIME_DIR"
  export XDG_RUNTIME_DIR
  unset DISPLAY WAYLAND_DISPLAY SWAYSOCK I3SOCK
  # headless: no DRM or input device; pixman: no GPU (a hosted runner has
  # none, and llvmpipe's GLES would only be slower).
  WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    sway --unsupported-gpu --config "$here/sway.conf" \
    >"${E2E_LOG_DIR:-/tmp}/sway.log" 2>&1 &
  wm=$!
  for _ in $(seq 1 100); do
    sock=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s -name 'wayland-*' | head -1)
    ipc=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s -name 'sway-ipc.*' | head -1)
    if [ -n "$sock" ] && [ -n "$ipc" ] && SWAYSOCK=$ipc swaymsg -t get_version >/dev/null 2>&1; then
      break
    fi
    sleep 0.2
  done
  [ -n "${sock:-}" ] && [ -n "${ipc:-}" ] || {
    echo "::error::sway did not start; sway.log:" >&2
    tail -40 "${E2E_LOG_DIR:-/tmp}/sway.log" >&2 || true
    kill "$wm" 2>/dev/null
    exit 1
  }
  export WAYLAND_DISPLAY SWAYSOCK=$ipc
  WAYLAND_DISPLAY=$(basename "$sock")
  # The seat's keyboard, and how the suite presses keys (input.ts).
  export E2E_WAYLAND_KEYBOARD="$XDG_RUNTIME_DIR/e2e-keyboard.sock"
  python3 "$here/wayland-input.py" "$E2E_WAYLAND_KEYBOARD" \
    >"${E2E_LOG_DIR:-/tmp}/wayland-input.log" 2>&1 &
  kbd=$!
  for _ in $(seq 1 50); do
    grep -q ready "${E2E_LOG_DIR:-/tmp}/wayland-input.log" && break
    sleep 0.2
  done
  grep -q ready "${E2E_LOG_DIR:-/tmp}/wayland-input.log" || {
    echo "::error::the session's keyboard did not start:" >&2
    cat "${E2E_LOG_DIR:-/tmp}/wayland-input.log" >&2
    kill "$kbd" "$wm" 2>/dev/null
    exit 1
  }
  wm_ready() { swaymsg -t get_version >/dev/null 2>&1; }
  wm_name() { swaymsg -t get_version 2>&1 | head -1; }
else
  xfwm4 --compositor=off --sm-client-disable >"${E2E_LOG_DIR:-/tmp}/xfwm4.log" 2>&1 &
  wm=$!
  wm_ready() { wmctrl -m >/dev/null 2>&1; }
  wm_name() { wmctrl -m 2>&1 | head -1; }
fi
export E2E_NOTIFY_LOG=${E2E_NOTIFY_LOG:-${E2E_LOG_DIR:-/tmp}/notify.log}
: >"$E2E_NOTIFY_LOG"
python3 "$here/notification-server.py" >"${E2E_LOG_DIR:-/tmp}/notification-server.log" 2>&1 &
ns=$!
for _ in $(seq 1 50); do
  if wm_ready && grep -q ready "${E2E_LOG_DIR:-/tmp}/notification-server.log"; then
    break
  fi
  sleep 0.2
done
echo "window manager: $(wm_name)"
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
  # Wait for this daemon to own the name before asking it anything: a
  # get-property on an unowned name makes the bus activate the host's own
  # gnome-keyring (its default XDG dirs, no login keyring), which then holds
  # the name and answers "unknown".
  owned=
  for _ in $(seq 1 50); do
    owned=$(busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
      org.freedesktop.DBus NameHasOwner s org.freedesktop.secrets 2>/dev/null || true)
    [ "$owned" = "b true" ] && break
    sleep 0.2
  done
  locked=
  if [ "$owned" = "b true" ]; then
    for _ in $(seq 1 50); do
      locked=$(busctl --user get-property org.freedesktop.secrets \
        /org/freedesktop/secrets/aliases/default \
        org.freedesktop.Secret.Collection Locked 2>/dev/null || true)
      [ -n "$locked" ] && break
      sleep 0.2
    done
  fi
  echo "secret service: gnome-keyring, default collection Locked: ${locked:-unknown}"
  [ "$locked" = "b true" ] || {
    echo "::error::the locked-keyring session did not start (name owned: ${owned:-no}, Locked: ${locked:-unknown}); see keyring.log" >&2
    kill "$keyring" ${kbd:-} "$ns" "$wm" 2>/dev/null
    exit 1
  }
fi

status=0
"$@" || status=$?
[ -z "$keyring" ] || kill "$keyring" 2>/dev/null || true
kill ${kbd:-} "$ns" "$wm" 2>/dev/null || true
exit "$status"
