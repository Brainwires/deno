#!/usr/bin/env bash
# Assemble one runtime archive per backend (webview, cef) for a target.
#
# Archive layout (no wrapping directory), matching what the STOCK
# `deno desktop` CLI resolves:
#
#   <runtime lib>        libdenort.dylib | libdenort.so | denort.dll
#                        -> DENORT_DESKTOP_BIN=<root>/<runtime lib>
#   laufey/              -> LAUFEY_DEV_DIR=<root>/laufey
#     webview/build/laufey_webview.app          (macOS, webview)
#     webview/build/laufey_webview[.exe]        (Linux/Windows, webview)
#     cef/build/Release/laufey.app              (macOS, cef)
#     cef/build/Release/laufey[.exe] + libcef.* + resources  (Linux/Windows, cef)
#   BUILD_INFO.json      what was built from where
#   licenses/            deno, laufey and CEF licenses
#
# On Linux/Windows `deno desktop` copies the WHOLE directory holding the
# backend binary into the packaged app, so those directories hold only the
# backend's runtime files, never build-tree leftovers.
#
# Env: TARGET VERSION DENORT_DIR LAUFEY_TAR OUT_DIR DENO_VERSION DENO_SHA
#      LAUFEY_SHA CEF_VERSION RUN_URL
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${VERSION:?}" "${DENORT_DIR:?}" "${LAUFEY_TAR:?}" "${OUT_DIR:?}"
: "${DENO_VERSION:?}" "${DENO_SHA:?}" "${LAUFEY_SHA:?}" "${CEF_VERSION:?}"

export COPYFILE_DISABLE=1 # no AppleDouble ._ files in macOS tarballs

repo_root=$(cd "$here/../.." && pwd)
lib=$(runtime_lib_name "$TARGET")
ext=$(archive_ext "$TARGET")
denort_dir=$(unix_path "$DENORT_DIR")
laufey_tar=$(unix_path "$LAUFEY_TAR")
out_dir=$(unix_path "$OUT_DIR")
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)

work=$(mktemp -d)
mkdir -p "$work/laufey"
(cd "$work/laufey" && tar -xf "$laufey_tar")

test -f "$denort_dir/$lib" || { echo "missing $denort_dir/$lib" >&2; exit 1; }

for backend in webview cef; do
  name="deno-desktop-runtime-${VERSION}-${TARGET}-${backend}"
  root="$work/$name"
  mkdir -p "$root/laufey" "$root/licenses"
  cp "$denort_dir/$lib" "$root/$lib"
  case "$backend" in
    webview)
      test -d "$work/laufey/webview/build" || { echo "laufey stage has no webview/build" >&2; exit 1; }
      mkdir -p "$root/laufey/webview"
      cp -a "$work/laufey/webview/build" "$root/laufey/webview/build"
      ;;
    cef)
      test -d "$work/laufey/cef/build/Release" || { echo "laufey stage has no cef/build/Release" >&2; exit 1; }
      mkdir -p "$root/laufey/cef/build"
      cp -a "$work/laufey/cef/build/Release" "$root/laufey/cef/build/Release"
      ;;
  esac
  cp "$repo_root/LICENSE.md" "$root/licenses/deno-LICENSE.md"
  cp "$work/laufey/licenses/LICENSE" "$root/licenses/laufey-LICENSE"
  if [ "$backend" = cef ]; then
    cp "$work/laufey/licenses/LICENSE.cef" "$root/licenses/cef-LICENSE"
  fi

  cat >"$root/BUILD_INFO.json" <<EOF
{
  "name": "$name",
  "version": "$VERSION",
  "target": "$TARGET",
  "backend": "$backend",
  "deno": { "version": "$DENO_VERSION", "repository": "https://github.com/${GITHUB_REPOSITORY:-Brainwires/deno}", "sha": "$DENO_SHA" },
  "laufey": { "repository": "https://github.com/Brainwires/laufey", "sha": "$LAUFEY_SHA" },
  "cef": { "version": "$CEF_VERSION" },
  "env": { "DENORT_DESKTOP_BIN": "$lib", "LAUFEY_DEV_DIR": "laufey" },
  "build": { "run": "${RUN_URL:-}" }
}
EOF

  archive="$out_dir/$name.$ext"
  rm -f "$archive"
  if [ "$ext" = zip ]; then
    (cd "$root" && 7z a -tzip -bd -mx=9 "$(native_path "$archive")" . >/dev/null)
  else
    tar -czf "$archive" -C "$root" .
  fi
  echo "$(sha256_of "$archive")  $name.$ext" >"$archive.sha256"
  echo "assembled $name.$ext ($(size_of "$archive") bytes)"
  cat "$archive.sha256"
done

rm -rf "$work"
