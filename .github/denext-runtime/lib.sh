# Shared helpers for the denext runtime scripts. Source, don't execute.

# An assertion that fails with a message naming what it checked, instead of
# `set -e`'s silent exit on a bare `test` / `[`:
#   need "the runtime library in the archive" test -f "$dir/$lib"
need() {
  local what=$1
  shift
  "$@" || { echo "::error::check failed: $what ($*)" >&2; exit 1; }
}

# The runtime library `deno desktop` reads through DENORT_DESKTOP_BIN.
runtime_lib_name() {
  case "$1" in
    *-apple-darwin) echo libdenort.dylib ;;
    *-linux-gnu) echo libdenort.so ;;
    *-windows-msvc) echo denort.dll ;;
    *) echo "unknown target: $1" >&2; return 1 ;;
  esac
}

# The binary in a `deno-cli-*` archive (assemble_cli.sh).
cli_bin_name() {
  case "$1" in
    *-windows-msvc) echo deno.exe ;;
    *) echo deno ;;
  esac
}

archive_ext() {
  case "$1" in
    *-windows-msvc) echo zip ;;
    *) echo tar.gz ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

size_of() {
  wc -c <"$1" | tr -d ' '
}

# A path the native (non-MSYS) programs on this runner understand.
native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    echo "$1"
  fi
}

# A POSIX path for the MSYS tools (GNU tar reads "D:/x" as host "D").
unix_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    echo "$1"
  fi
}

# The JSON a file holds (toolchain_info.py's record), for a BUILD_INFO.json
# heredoc, or `null` when there is no such file (no path given, or artifacts
# reused from a run that recorded none).
json_or_null() {
  local f=""
  if [ -n "${1:-}" ]; then f=$(unix_path "$1"); fi
  if [ -n "$f" ] && [ -s "$f" ]; then
    cat "$f"
  else
    echo null
  fi
}

# Unpack an archive produced by assemble.sh into an (empty) directory.
unpack_archive() {
  local archive=$1 dest=$2
  mkdir -p "$dest"
  case "$archive" in
    *.zip) 7z x -y -bd -o"$(native_path "$dest")" "$(native_path "$archive")" >/dev/null ;;
    *.tar.gz) tar -xzf "$archive" -C "$dest" ;;
    *) echo "unknown archive type: $archive" >&2; return 1 ;;
  esac
}

# Wait for a background process to write a non-empty file: up to 60 s (a CI
# macOS runner can take many seconds to start Python), and not past its exit.
wait_for_file() { # <file> <pid>
  local deadline=$((SECONDS + 60))
  while [ "$SECONDS" -lt "$deadline" ]; do
    test -s "$1" && return 0
    kill -0 "$2" 2>/dev/null || { test -s "$1"; return; }
    sleep 0.1
  done
  return 1
}
