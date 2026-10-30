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
# Every binary named must have its debug file: one without is an error
# (DENEXT.md item 181; a release whose crash traces can't be symbolicated is
# not one to publish). Cargo leaves macOS's `<bin>.dSYM` in target/release as
# a symlink into deps/, so the archive follows links (`tar -h`): denext.2 and
# denext.3 shipped macOS archives holding only the dangling symlinks. A
# `.dSYM` must hold its DWARF file.
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
  [ -n "$hit" ] || { echo "::error::no debug info for $b in $rel (looked for $b.dSYM, $b.dwp, ${b%.so}.dwp, $stem.pdb)" >&2; exit 1; }
  last=${found[${#found[@]}-1]}
  case "$last" in
    *.dSYM)
      # Named after the binary Cargo linked (deps/deno-<hash>), not <bin>.
      dwarf=$(find -L "$rel/$last/Contents/Resources/DWARF" -type f -size +0 2>/dev/null | head -1)
      [ -n "$dwarf" ] || { echo "::error::$last has no DWARF file (in $rel/$last/Contents/Resources/DWARF)" >&2; exit 1; }
      ;;
  esac
done
mkdir -p "$out"
# -h: follow the .dSYM symlinks into deps/.
tar -czhf "$out/$name.tar.gz" -C "$rel" "${found[@]}"
echo "$(sha256_of "$out/$name.tar.gz")  $name.tar.gz" >"$out/$name.tar.gz.sha256"
echo "$name.tar.gz ($(size_of "$out/$name.tar.gz") bytes): ${found[*]}"
