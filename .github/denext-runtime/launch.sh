#!/usr/bin/env bash
# Launch the app smoke.sh packaged and wait for the page to report back.
#
# The page POSTs to its own origin; the server writes $SMOKE_RESULT_FILE and
# exits 0. Success = the process exits and the result file exists.
#
# The environment a denext launcher provides is set here explicitly: the app
# id (per-app web storage) and, for CEF, the custom scheme, which CEF must
# know at process start and a stock `deno desktop` does not write into the
# packaged app. A direct exec also bypasses LaunchServices, so a macOS
# bundle's LSEnvironment would not apply anyway.
#
# When smoke.sh embedded the Node-API probe addon, the app loads it and the
# result must show both of its functions working.
#
# Env: TARGET BACKEND WORK_DIR
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${BACKEND:?}" "${WORK_DIR:?}"
work=$(unix_path "$WORK_DIR")
out="$work/app/out"

case "$TARGET" in
  *-apple-darwin)
    bundle=$(find "$out" -maxdepth 1 -name '*.app' | head -1)
    exe_name=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$bundle/Contents/Info.plist")
    exe="$bundle/Contents/MacOS/$exe_name"
    ;;
  *-windows-msvc)
    exe=$(find "$out" -maxdepth 2 -iname 'smoke.exe' | head -1)
    ;;
  *)
    exe=$(find "$out" -maxdepth 2 -type f -name smoke | head -1)
    ;;
esac
test -n "$exe" && test -f "$exe" || { echo "no packaged executable under $out" >&2; exit 1; }
echo "launching $exe"

result="$work/result.json"
rm -f "$result"
export SMOKE_RESULT_FILE
SMOKE_RESULT_FILE=$(native_path "$result")
export LAUFEY_APP_ID=dev.denext.smoke
napi=0
if [ -f "$work/app/probe.node" ]; then
  napi=1
fi
export SMOKE_NAPI=$napi
if [ "$BACKEND" = cef ]; then
  export LAUFEY_CUSTOM_SCHEMES=t3code
fi

limit=120
status=0
case "$TARGET" in
  *-linux-gnu)
    export WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
    timeout "$limit" xvfb-run --auto-servernum --server-args="-screen 0 1280x800x24" \
      dbus-run-session -- "$exe" || status=$?
    ;;
  *-apple-darwin)
    # No coreutils `timeout` on the macOS images.
    perl -e 'alarm shift; exec @ARGV' "$limit" "$exe" || status=$?
    ;;
  *)
    timeout "$limit" "$exe" || status=$?
    ;;
esac
echo "exit status: $status"

if [ -f "$result" ]; then
  echo "--- result"
  cat "$result"
  echo
fi
[ "$status" = 0 ] && [ -f "$result" ] || { echo "launch smoke FAILED" >&2; exit 1; }
if [ "$napi" = 1 ]; then
  # {"add": 42, "lookup": "napi <n>"}: linked and run-time-resolved Node-API
  # calls both reached the runtime library.
  grep -Eq '"add": 42' "$result" && grep -Eq '"lookup": "napi [0-9]+"' "$result" ||
    { echo "Node-API probe FAILED" >&2; exit 1; }
  echo "Node-API probe passed"
fi
echo "launch smoke passed"
