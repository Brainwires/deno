#!/usr/bin/env bash
# Assemble the `deno` CLI archive for a target: this fork's own `deno` binary
# (the CLI-side features, e.g. Vitest compatibility in `deno test` and
# `Deno.SQL`), built with `cargo build --release --locked --bin deno`.
#
# A zip on every target (what denext downloads), no wrapping directory:
#
#   deno | deno.exe        the CLI
#   esbuild | esbuild.exe  the official esbuild binary `deno bundle` uses
#                          instead of downloading one (fetch_esbuild.py)
#   esbuild.LICENSE        esbuild's MIT license
#   typescript-lsp/        the pinned TypeScript 7 build `deno lsp`'s tsgo
#                          backend runs (fetch_typescript.py lsp; tsc, its
#                          lib.*.d.ts, LICENSE, NOTICE.txt, typescript.json)
#   typescript-check/      the pinned TypeScript 7 build `deno check` and every
#                          other type-checking command run (fetch_typescript.py
#                          check; the same layout)
#   tsgolint/              the pinned tsgolint `deno lint --type-aware` runs
#                          (fetch_tsgolint.py; tsgolint, LICENSE,
#                          tsgolint.json)
#   BUILD_INFO.json        what was built from where (esbuild's source too)
#   licenses/              the deno license
#
# The `denort` archive is assemble_denort.sh's: it is built first, in its own
# job, so this CLI is compiled with its SHA-256 (DENEXT.md item 188).
#
# Env: TARGET VERSION DENO_BIN ESBUILD_DIR TYPESCRIPT_LSP_DIR
# TYPESCRIPT_CHECK_DIR TSGOLINT_DIR OUT_DIR DENO_VERSION DENO_SHA RUN_URL
# (ESBUILD_DIR: fetch_esbuild.py's output directory; TYPESCRIPT_LSP_DIR /
# TYPESCRIPT_CHECK_DIR: fetch_typescript.py lsp's / check's; TSGOLINT_DIR:
# fetch_tsgolint.py's)
# (VERSION: the fork's own version, e.g. 3.0.0-denext.1; DENO_VERSION: the
# upstream Deno release it is built from, DENEXT.md "Versioning")
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${VERSION:?}" "${DENO_BIN:?}" "${ESBUILD_DIR:?}" "${TYPESCRIPT_LSP_DIR:?}" "${TYPESCRIPT_CHECK_DIR:?}" "${TSGOLINT_DIR:?}" "${OUT_DIR:?}"
: "${DENO_VERSION:?}" "${DENO_SHA:?}"

export COPYFILE_DISABLE=1 # no AppleDouble ._ files in macOS tarballs

repo_root=$(cd "$here/../.." && pwd)
bin=$(cli_bin_name "$TARGET")
deno_bin=$(unix_path "$DENO_BIN")
test -f "$deno_bin" || { echo "missing $deno_bin" >&2; exit 1; }
esbuild_dir=$(unix_path "$ESBUILD_DIR")
case "$TARGET" in
  *-windows-msvc) esbuild_bin=esbuild.exe ;;
  *) esbuild_bin=esbuild ;;
esac
for f in "$esbuild_bin" esbuild.LICENSE esbuild.json; do
  test -f "$esbuild_dir/$f" || { echo "missing $esbuild_dir/$f (run fetch_esbuild.py)" >&2; exit 1; }
done
# The binary must be the one fetch_esbuild.py verified.
esbuild_sha=$(sha256_of "$esbuild_dir/$esbuild_bin")
grep -q "\"sha256\": \"$esbuild_sha\"" "$esbuild_dir/esbuild.json" ||
  { echo "$esbuild_dir/$esbuild_bin does not match esbuild.json" >&2; exit 1; }
ts_lsp_dir=$(unix_path "$TYPESCRIPT_LSP_DIR")
case "$TARGET" in
  *-windows-msvc) tsc_bin=tsc.exe ;;
  *) tsc_bin=tsc ;;
esac
for f in "$tsc_bin" LICENSE NOTICE.txt typescript.json; do
  test -f "$ts_lsp_dir/$f" || { echo "missing $ts_lsp_dir/$f (run fetch_typescript.py lsp)" >&2; exit 1; }
done
# The binary must be the one fetch_typescript.py verified.
tsc_sha=$(sha256_of "$ts_lsp_dir/$tsc_bin")
grep -q "\"sha256\": \"$tsc_sha\"" "$ts_lsp_dir/typescript.json" ||
  { echo "$ts_lsp_dir/$tsc_bin does not match typescript.json" >&2; exit 1; }
ts_check_dir=$(unix_path "$TYPESCRIPT_CHECK_DIR")
for f in "$tsc_bin" LICENSE NOTICE.txt typescript.json; do
  test -f "$ts_check_dir/$f" || { echo "missing $ts_check_dir/$f (run fetch_typescript.py check)" >&2; exit 1; }
done
tsc_check_sha=$(sha256_of "$ts_check_dir/$tsc_bin")
grep -q "\"sha256\": \"$tsc_check_sha\"" "$ts_check_dir/typescript.json" ||
  { echo "$ts_check_dir/$tsc_bin does not match typescript.json" >&2; exit 1; }
tsgolint_dir=$(unix_path "$TSGOLINT_DIR")
case "$TARGET" in
  *-windows-msvc) tsgolint_bin=tsgolint.exe ;;
  *) tsgolint_bin=tsgolint ;;
esac
for f in "$tsgolint_bin" LICENSE tsgolint.json; do
  test -f "$tsgolint_dir/$f" || { echo "missing $tsgolint_dir/$f (run fetch_tsgolint.py)" >&2; exit 1; }
done
# The binary must be the one fetch_tsgolint.py verified.
tsgolint_sha=$(sha256_of "$tsgolint_dir/$tsgolint_bin")
grep -q "\"sha256\": \"$tsgolint_sha\"" "$tsgolint_dir/tsgolint.json" ||
  { echo "$tsgolint_dir/$tsgolint_bin does not match tsgolint.json" >&2; exit 1; }
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
cp "$esbuild_dir/$esbuild_bin" "$root/$esbuild_bin"
chmod +x "$root/$esbuild_bin"
cp "$esbuild_dir/esbuild.LICENSE" "$root/esbuild.LICENSE"
mkdir -p "$root/typescript-lsp"
cp -R "$ts_lsp_dir/." "$root/typescript-lsp/"
chmod +x "$root/typescript-lsp/$tsc_bin"
mkdir -p "$root/typescript-check"
cp -R "$ts_check_dir/." "$root/typescript-check/"
chmod +x "$root/typescript-check/$tsc_bin"
mkdir -p "$root/tsgolint"
cp -R "$tsgolint_dir/." "$root/tsgolint/"
chmod +x "$root/tsgolint/$tsgolint_bin"

cat >"$root/BUILD_INFO.json" <<EOF
{
  "name": "$name",
  "version": "$VERSION",
  "target": "$TARGET",
  "binary": "$bin",
  "deno": { "version": "$DENO_VERSION", "repository": "https://github.com/${GITHUB_REPOSITORY:-Brainwires/deno}", "sha": "$DENO_SHA" },
  "esbuild": $(cat "$esbuild_dir/esbuild.json"),
  "typescript_lsp": $(cat "$ts_lsp_dir/typescript.json"),
  "typescript_check": $(cat "$ts_check_dir/typescript.json"),
  "tsgolint": $(cat "$tsgolint_dir/tsgolint.json"),
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

