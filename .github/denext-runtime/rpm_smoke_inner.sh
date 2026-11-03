#!/usr/bin/env bash
# Inside a Fedora container, as root: install /rpm/smoke.rpm the way a user
# would, check every library its ELF files link resolves, then start the app
# headless and wait for its page to report back (smoke/main.ts).
#
# The test tools are installed only after the library check, so what they
# pull in can't hide a library the package failed to bring. Weak dependencies
# stay out for the same reason.
#
# Env: BACKEND ORIGIN
set -euo pipefail
: "${BACKEND:?}" "${ORIGIN:?}"
dnf_install() { dnf install -y -q --setopt=install_weak_deps=False "$@"; }

. /etc/os-release
echo "--- $PRETTY_NAME, $(uname -m), $BACKEND"
echo "--- install"
if ! dnf_install /rpm/smoke.rpm; then
  echo "::error::dnf could not install the .rpm on $PRETTY_NAME ($BACKEND)" >&2
  exit 1
fi

echo "--- the libraries of the installed files"
missing=0
while IFS= read -r -d '' f; do
  head -c4 "$f" | grep -q ELF || continue
  out=$(LD_LIBRARY_PATH=$(dirname "$f") ldd "$f" 2>&1 | grep 'not found' || true)
  if [ -n "$out" ]; then
    echo "${f}:"
    echo "$out"
    missing=1
  fi
done < <(find /usr/lib/smoke -type f -print0)
if [ "$missing" != 0 ]; then
  echo "::error::the .rpm's dependencies leave libraries unresolved on $PRETTY_NAME ($BACKEND)" >&2
  exit 1
fi
echo "all resolved"

echo "--- start"
dnf_install xorg-x11-server-Xvfb xorg-x11-xauth dbus-daemon util-linux shadow-utils which
useradd smoke
result=/home/smoke/result.json
status=0
runuser -u smoke -- env \
  SMOKE_RESULT_FILE="$result" \
  WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
  timeout 120 xvfb-run --auto-servernum --server-args="-screen 0 1280x800x24" \
  dbus-run-session -- /usr/bin/smoke || status=$?
echo "exit status: $status"
if [ ! -f "$result" ]; then
  echo "::error::the installed app never reported back on $PRETTY_NAME ($BACKEND)" >&2
  exit 1
fi
cat "$result"
echo
if ! grep -qF "\"origin\": \"$ORIGIN\"" "$result" || ! grep -q '"secureContext": true' "$result"; then
  echo "::error::the page was not served from the app origin in a secure context" >&2
  exit 1
fi
if [ "$status" != 0 ]; then
  echo "::error::the app reported back, then exited with $status" >&2
  exit 1
fi

echo "--- remove"
rpm -e smoke
[ ! -e /usr/lib/smoke ] || { echo "::error::rpm -e left /usr/lib/smoke" >&2; exit 1; }
echo "rpm smoke passed on $PRETTY_NAME ($BACKEND)"
