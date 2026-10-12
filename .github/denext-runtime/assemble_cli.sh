#!/usr/bin/env bash
# Assemble the `deno` CLI archive for a target: this fork's own `deno` binary
# (the CLI-side features, e.g. Vitest compatibility in `deno test` and
# `Deno.SQL`), built with `cargo build --release --locked --bin deno`.
#
# A zip on every target (what denext downloads), no wrapping directory:
#
#   deno | deno.exe      the CLI
#   BUILD_INFO.json      what was built from where
#   licenses/            the deno license
#
# Env: TARGET VERSION DENO_BIN OUT_DIR DENO_VERSION DENO_SHA RUN_URL
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${VERSION:?}" "${DENO_BIN:?}" "${OUT_DIR:?}"
: "${DENO_VERSION:?}" "${DENO_SHA:?}"

export COPYFILE_DISABLE=1 # no AppleDouble ._ files in macOS tarballs

repo_root=$(cd "$here/../.." && pwd)
bin=$(cli_bin_name "$TARGET")
deno_bin=$(unix_path "$DENO_BIN")
test -f "$deno_bin" || { echo "missing $deno_bin" >&2; exit 1; }
out_dir=$(unix_path "$OUT_DIR")
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)

name="deno-cli-${VERSION}-${TARGET}"
archive="$out_dir/$name.zip"
test ! -e "$archive" || { echo "$archive already exists" >&2; exit 1; }

work=$(mktemp -d)
root="$work/$name"
mkdir -p "$root/licenses"
cp "$deno_bin" "$root/$bin"
chmod +x "$root/$bin"
cp "$repo_root/LICENSE.md" "$root/licenses/deno-LICENSE.md"

cat >"$root/BUILD_INFO.json" <<EOF
{
  "name": "$name",
  "version": "$VERSION",
  "target": "$TARGET",
  "binary": "$bin",
  "deno": { "version": "$DENO_VERSION", "repository": "https://github.com/${GITHUB_REPOSITORY:-Brainwires/deno}", "sha": "$DENO_SHA" },
  "build": { "run": "${RUN_URL:-}" }
}
EOF

if command -v zip >/dev/null 2>&1; then
  # Info-ZIP keeps the binary's executable bit.
  (cd "$root" && zip -q -9 -r "$archive" .)
else
  (cd "$root" && 7z a -tzip -bd -mx=9 "$(native_path "$archive")" . >/dev/null)
fi
echo "$(sha256_of "$archive")  $name.zip" >"$archive.sha256"
echo "assembled $name.zip ($(size_of "$archive") bytes)"
cat "$archive.sha256"
