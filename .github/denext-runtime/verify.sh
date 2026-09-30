#!/usr/bin/env bash
# Check the binaries in an unpacked runtime archive: architecture matches the
# target, and every dynamic dependency resolves (on macOS: to the system, or
# to a path inside the archive; never to a build-machine path such as
# Homebrew). Prints the evidence (file / lipo / otool / ldd) either way.
#
# Usage: verify.sh <target> <unpacked archive dir>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"
target=$1
root=$(unix_path "$2")
fail=0

macho_files() {
  find "$root" -type f \( -name '*.dylib' -o -perm -u+x \) -print0 |
    while IFS= read -r -d '' f; do
      if file -b "$f" | grep -q 'Mach-O'; then printf '%s\n' "$f"; fi
    done
}

check_macos() {
  local want
  case "$target" in
    x86_64-*) want=x86_64 ;;
    aarch64-*) want=arm64 ;;
  esac
  while IFS= read -r f; do
    rel=${f#"$root"/}
    archs=$(lipo -archs "$f")
    echo "== $rel"
    echo "   file: $(file -b "$f")"
    echo "   lipo: $archs"
    if [ "$archs" != "$want" ]; then
      echo "   ERROR: expected $want" >&2
      fail=1
    fi
    rpaths=$(otool -l "$f" | awk '/cmd LC_RPATH/{getline; getline; print $2}')
    dir=$(dirname "$f")
    # Skip the first line (the file name) and, for a dylib, its own id.
    otool -L "$f" | tail -n +2 | awk '{print $1}' | while read -r dep; do
      resolved=""
      case "$dep" in
        /System/* | /usr/lib/*) resolved=system ;;
        @loader_path/* | @executable_path/*)
          # The executable of a bundle lives next to its dylibs' loader here;
          # resolve both relative to the file's own directory.
          p="$dir/${dep#*/}"
          [ -e "$p" ] && resolved="$p"
          ;;
        @rpath/*)
          for rp in $rpaths; do
            base=${rp/@loader_path/$dir}
            base=${base/@executable_path/$dir}
            [ -e "$base/${dep#@rpath/}" ] && resolved="$base/${dep#@rpath/}" && break
          done
          ;;
        *) [ -e "$dep" ] && resolved="$dep (build-machine path!)" ;;
      esac
      case "$dep" in
        /System/* | /usr/lib/* | @*) ;;
        *)
          echo "   ERROR: non-system absolute dependency $dep" >&2
          echo 1 >"$root/.verify-failed"
          ;;
      esac
      echo "   dep: $dep -> ${resolved:-UNRESOLVED (loaded at runtime or via framework search)}"
    done
  done < <(macho_files)
  # The CEF framework binary is not executable-bit-marked everywhere; show it.
  find "$root" -path '*Chromium Embedded Framework.framework/Chromium Embedded Framework' -type f |
    while IFS= read -r f; do
      echo "== ${f#"$root"/}: $(lipo -archs "$f")"
      [ "$(lipo -archs "$f")" = "$want" ] || { echo "   ERROR: expected $want" >&2; echo 1 >"$root/.verify-failed"; }
    done
  echo "== codesign (libdenort)"
  codesign -dv "$root/libdenort.dylib" 2>&1 | sed 's/^/   /' || true
}

check_linux() {
  local want
  case "$target" in
    x86_64-*) want=x86-64 ;;
    aarch64-*) want=aarch64 ;;
  esac
  find "$root" -type f \( -name '*.so' -o -name '*.so.*' -o -perm -u+x \) -print0 |
    while IFS= read -r -d '' f; do
      file -b "$f" | grep -q '^ELF' || continue
      rel=${f#"$root"/}
      echo "== $rel"
      echo "   file: $(file -b "$f")"
      if ! file -b "$f" | grep -q "$want"; then
        echo "   ERROR: expected $want" >&2
        echo 1 >"$root/.verify-failed"
      fi
      glibc=$(objdump -T "$f" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1 || true)
      echo "   max glibc symbol: ${glibc:-none}"
      missing=$(LD_LIBRARY_PATH=$(dirname "$f") ldd "$f" 2>&1 | grep 'not found' || true)
      if [ -n "$missing" ]; then
        echo "   ERROR: unresolved: $missing" >&2
        echo 1 >"$root/.verify-failed"
      fi
    done
}

check_windows() {
  find "$root" -type f \( -iname '*.exe' -o -iname '*.dll' \) -print0 |
    while IFS= read -r -d '' f; do
      rel=${f#"$root"/}
      echo "== $rel: $(file -b "$f" 2>/dev/null || echo '?')"
    done
}

rm -f "$root/.verify-failed"
case "$target" in
  *-apple-darwin) check_macos ;;
  *-linux-gnu) check_linux ;;
  *-windows-msvc) check_windows ;;
esac
if [ -f "$root/.verify-failed" ] || [ "$fail" != 0 ]; then
  rm -f "$root/.verify-failed"
  echo "verify FAILED for $target" >&2
  exit 1
fi
echo "verify passed for $target"
