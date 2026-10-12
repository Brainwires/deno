#!/usr/bin/env bash
# Assemble the `deno` CLI archive for a target: this fork's own `deno` binary
# (the CLI-side features, e.g. Vitest compatibility in `deno test` and
# `Deno.SQL`), built with `cargo build --release --locked --bin deno`.
#
# Archive layout (no wrapping directory):
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
ext=$(archive_ext "$TARGET")
deno_bin=$(unix_path "$DENO_BIN")
test -f "$deno_bin" || { echo "missing $deno_bin" >&2; exit 1; }
out_dir=$(unix_path "$OUT_DIR")
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)

name="deno-cli-${VERSION}-${TARGET}"
archive="$out_dir/$name.$ext"
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

if [ "$ext" = zip ]; then
  (cd "$root" && 7z a -tzip -bd -mx=9 "$(native_path "$archive")" . >/dev/null)
else
  tar -czf "$archive" -C "$root" .
fi
echo "$(sha256_of "$archive")  $name.$ext" >"$archive.sha256"
echo "assembled $name.$ext ($(size_of "$archive") bytes)"
cat "$archive.sha256"
