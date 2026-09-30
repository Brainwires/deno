# Shared helpers for the denext runtime scripts. Source, don't execute.

# The runtime library `deno desktop` reads through DENORT_DESKTOP_BIN.
runtime_lib_name() {
  case "$1" in
    *-apple-darwin) echo libdenort.dylib ;;
    *-linux-gnu) echo libdenort.so ;;
    *-windows-msvc) echo denort.dll ;;
    *) echo "unknown target: $1" >&2; return 1 ;;
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
