#!/usr/bin/env bash
# Package the app smoke.sh left in $WORK_DIR as a real `.rpm` with denext's
# packager, then install and start it in a Fedora container
# (rpm_smoke_inner.sh). Needs rpmbuild and docker on the host. See DENEXT.md
# item 233.
#
# Env: TARGET BACKEND WORK_DIR DENEXT_PACKAGING_VERSION [FEDORA_IMAGE]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${BACKEND:?}" "${WORK_DIR:?}" "${DENEXT_PACKAGING_VERSION:?}"
image=${FEDORA_IMAGE:-registry.fedoraproject.org/fedora:44}
work=$(unix_path "$WORK_DIR")
bundle="$work/app/out/smoke"
need "the packaged app from smoke.sh" test -x "$bundle/smoke"
case "$TARGET" in
  x86_64-*) arch=x86_64 ;;
  aarch64-*) arch=arm64 ;;
  *) echo "rpm_smoke.sh is for Linux targets, not $TARGET" >&2; exit 1 ;;
esac

origin=$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["origin"])' \
  "$work/app/.deno-desktop/app.json")

rm -rf "$work/rpm"
mkdir -p "$work/rpm"
deno run -q -A --no-config --no-lock "$here/rpm_smoke_build.ts" \
  "$bundle" "$arch" "$BACKEND" "$work/rpm/smoke.rpm" "$origin"
echo "--- the package's requirements"
rpm -qp --requires "$work/rpm/smoke.rpm" | grep -v '^rpmlib('

# CEF's setuid sandbox helper and WebKit's bubblewrap make namespaces, which
# docker's default seccomp profile and capability set refuse.
docker run --rm -i \
  --cap-add SYS_ADMIN --security-opt seccomp=unconfined --security-opt apparmor=unconfined \
  -v "$work/rpm:/rpm:ro" \
  -v "$here/rpm_smoke_inner.sh:/rpm_smoke_inner.sh:ro" \
  -e BACKEND="$BACKEND" -e ORIGIN="$origin" \
  "$image" bash /rpm_smoke_inner.sh </dev/null
