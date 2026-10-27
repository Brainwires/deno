#!/usr/bin/env bash
# Pack the split debug info the release profile leaves beside the binaries
# (Cargo.toml [profile.release]: split-debuginfo = "packed", debug =
# "line-tables-only") into <out dir>/<name>.tar.gz plus a .sha256, so a crash
# address from a shipped binary can be symbolicated (O1). The binaries
# themselves ship in the release archives; this is only their debug info:
#
#   macOS    <bin>.dSYM/
#   Linux    <bin>.dwp
#   Windows  <stem>.pdb
#
#   collect_symbols.sh <name> <out dir> <file in target/release>...
#
# Best effort: a binary with no debug file is named in a warning, never an
# error (the release does not depend on it).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

[ $# -ge 3 ] || { echo "::error::usage: collect_symbols.sh <name> <out dir> <file>..." >&2; exit 1; }
name=$1
out=$2
shift 2
rel=target/release
found=()
for b in "$@"; do
  stem=${b%.exe}
  stem=${stem%.dll}
  hit=
  for c in "$b.dSYM" "$b.dwp" "${b%.so}.dwp" "$stem.pdb"; do
    if [ -e "$rel/$c" ]; then
      found+=("$c")
      hit=1
      break
    fi
  done
  [ -n "$hit" ] || echo "::warning::no debug info for $b in $rel (looked for $b.dSYM, $b.dwp, ${b%.so}.dwp, $stem.pdb)"
done
if [ ${#found[@]} -eq 0 ]; then
  echo "::warning::no debug info found for $*: no $name.tar.gz"
  exit 0
fi
mkdir -p "$out"
tar -czf "$out/$name.tar.gz" -C "$rel" "${found[@]}"
echo "$(sha256_of "$out/$name.tar.gz")  $name.tar.gz" >"$out/$name.tar.gz.sha256"
echo "$name.tar.gz ($(size_of "$out/$name.tar.gz") bytes): ${found[*]}"
