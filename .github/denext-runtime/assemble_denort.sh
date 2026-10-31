#!/usr/bin/env bash
# Assemble the `denort` archive for a target, deno-denort-<version>-<target>.zip:
# just `denort` / `denort.exe` at the top, as Deno's own denort zips are, for
# `deno compile` (the CLI downloads it from the release:
# cli/standalone/binary.rs, denext_denort). Built with `cargo build --release
# --locked --bin denort` in the workflow's `denort_bin` job, before the CLI:
# the CLI is compiled with every target's archive SHA-256
# (DENEXT_DENORT_SHA256S, DENEXT.md item 187), so the archive made here is the
# one released (the `cli` job copies it into its artifact unchanged).
#
# Env: TARGET VERSION DENORT_BIN OUT_DIR
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${VERSION:?}" "${DENORT_BIN:?}" "${OUT_DIR:?}"

export COPYFILE_DISABLE=1

denort_bin=$(unix_path "$DENORT_BIN")
test -f "$denort_bin" || { echo "missing $denort_bin" >&2; exit 1; }
out_dir=$(unix_path "$OUT_DIR")
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)
work=$(mktemp -d)
rt_name="deno-denort-${VERSION}-${TARGET}"
rt_archive="$out_dir/$rt_name.zip"
test ! -e "$rt_archive" || { echo "$rt_archive already exists" >&2; exit 1; }
case "$TARGET" in
  *-windows-msvc) rt_bin=denort.exe ;;
  *) rt_bin=denort ;;
esac
rt_root="$work/$rt_name"
mkdir -p "$rt_root"
cp "$denort_bin" "$rt_root/$rt_bin"
chmod +x "$rt_root/$rt_bin"
if command -v zip >/dev/null 2>&1; then
  (cd "$rt_root" && zip -q -9 -r "$rt_archive" .)
else
  (cd "$rt_root" && 7z a -tzip -bd -mx=9 "$(native_path "$rt_archive")" . >/dev/null)
fi
echo "$(sha256_of "$rt_archive")  $rt_name.zip" >"$rt_archive.sha256"
echo "assembled $rt_name.zip ($(size_of "$rt_archive") bytes)"
cat "$rt_archive.sha256"
