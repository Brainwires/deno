#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Check that shipped Linux ELF files declare only the libraries we expect.

Reads each file's DT_NEEDED entries (`readelf -d`) and fails on a library the
file is not listed for, and on an ELF file with no entry here. A new link
dependency that a distro lacks then fails the release instead of the users'
launches, and adding it to EXPECTED makes the author decide how a package gets
it: from denext's `.deb` / `.rpm` dependency lists (PACKAGE_DEPS, a copy of
LINUX_RUNTIME_DEPS in denext's src/build/desktop-installers.ts) or from what
those already pull in (TRANSITIVE, which rpm_smoke.sh checks on Fedora).
linux_needed_test.py keeps the tables consistent with each other. The lists
started from the 3.0.0-denext.7 archives of both architectures.

A listed library a file no longer needs is only reported: it can go from the
table, and nothing breaks for a user meanwhile.

Usage: linux_needed.py <directory or file>...
A directory is walked, its files named relative to it; a file is named by its
base name. READELF names the readelf to run (default: readelf). Exits 0 when
everything is expected, 1 otherwise, 2 on a usage error. verify.sh runs it on
every Linux runtime archive, and denext_runtime.yml's denort_bin and cli jobs
on their binaries (DENEXT.md item 2NN).
"""

import os
import re
import subprocess
import sys

# glibc and the compiler runtime, which any file may link. libstdc++ is not
# here: `deno` and `denort` must stay free of it.
CORE = {
  "libc.so.6",
  "libdl.so.2",
  "libgcc_s.so.1",
  "libm.so.6",
  "libpthread.so.0",
  "librt.so.1",
}
# The dynamic loader's name differs per architecture.
LOADER = re.compile(r"^ld-linux-[a-z0-9-]+\.so\.\d+$")

# The libraries a package names, per backend, by soname.
PACKAGE_DEPS = {
  "webview": {"libwebkit2gtk-4.1.so.0", "libgtk-3.so.0"},
  "cef": {
    "libgtk-3.so.0",
    "libnss3.so",
    "libasound.so.2",
    "libX11.so.6",
    "libXcomposite.so.1",
    "libXdamage.so.1",
    "libXext.so.6",
    "libXfixes.so.3",
    "libXrandr.so.2",
    "libgbm.so.1",
    "libxkbcommon.so.0",
    "libpango-1.0.so.0",
    "libcairo.so.2",
    "libatk-1.0.so.0",
    "libdbus-1.so.3",
    "libexpat.so.1",
    "libxcb.so.1",
    "libdrm.so.2",
  },
}
# Named by the packages though nothing links it directly (libgbm does).
DECLARED_NOT_NEEDED = {"cef": {"libdrm.so.2"}, "webview": set()}

# Linked directly, but reaching an installed app through a package above.
_GTK_CLOSURE = {
  "libcairo.so.2",
  "libgdk-3.so.0",
  "libgdk_pixbuf-2.0.so.0",
  "libgio-2.0.so.0",
  "libglib-2.0.so.0",
  "libgobject-2.0.so.0",
  "libstdc++.so.6",
  "libudev.so.1",
  "libwayland-client.so.0",
  "libxcb.so.1",
  "libz.so.1",
}
TRANSITIVE = {
  "webview": _GTK_CLOSURE | {
    "libdbus-1.so.3",
    "libjavascriptcoregtk-4.1.so.0",
    "libsoup-3.0.so.0",
  },
  "cef": _GTK_CLOSURE | {
    "libXi.so.6",
    "libatk-bridge-2.0.so.0",
    "libatspi.so.0",
    "libcups.so.2",
    "libnspr4.so",
    "libnssutil3.so",
    "libsmime3.so",
  },
}
# Shipped beside the file that links it.
BUNDLED = {"libcef.so"}

_WEBAUTHN = {"libudev.so.1", "libdbus-1.so.3"}
_WEBVIEW = "laufey/webview/build"
_CEF = "laufey/cef/build/Release"

# File -> what it may link beyond CORE and the loader. Keys under _WEBVIEW and
# _CEF belong to that backend; the others to both, or to no package.
EXPECTED = {
  # The toolchain archives: nothing a distro could lack.
  "deno": set(),
  "denort": set(),
  "esbuild": set(),
  "tsgolint/tsgolint": set(),
  "typescript-check/tsc": set(),
  "typescript-lsp/tsc": set(),
  # Both runtime archives.
  "libdenort.so": {"libz.so.1"},
  f"{_WEBVIEW}/laufey_webview": {
    "libcairo.so.2",
    "libgdk-3.so.0",
    "libgdk_pixbuf-2.0.so.0",
    "libgio-2.0.so.0",
    "libglib-2.0.so.0",
    "libgobject-2.0.so.0",
    "libgtk-3.so.0",
    "libjavascriptcoregtk-4.1.so.0",
    "libsoup-3.0.so.0",
    "libstdc++.so.6",
    "libwayland-client.so.0",
    "libwebkit2gtk-4.1.so.0",
    "libxcb.so.1",
  },
  # libdbus only on aarch64.
  f"{_WEBVIEW}/liblaufey_webauthn.so": _WEBAUTHN,
  f"{_CEF}/liblaufey_webauthn.so": _WEBAUTHN,
  f"{_CEF}/laufey": {
    "libX11.so.6",
    "libXi.so.6",
    "libcairo.so.2",
    "libcef.so",
    "libgdk-3.so.0",
    "libgdk_pixbuf-2.0.so.0",
    "libgio-2.0.so.0",
    "libglib-2.0.so.0",
    "libgobject-2.0.so.0",
    "libgtk-3.so.0",
    "libstdc++.so.6",
    "libwayland-client.so.0",
    "libxcb.so.1",
  },
  f"{_CEF}/libcef.so": {
    "libX11.so.6",
    "libXcomposite.so.1",
    "libXdamage.so.1",
    "libXext.so.6",
    "libXfixes.so.3",
    "libXrandr.so.2",
    "libasound.so.2",
    "libatk-1.0.so.0",
    "libatk-bridge-2.0.so.0",
    "libatspi.so.0",
    "libcairo.so.2",
    "libcups.so.2",
    "libdbus-1.so.3",
    "libexpat.so.1",
    "libgbm.so.1",
    "libgio-2.0.so.0",
    "libglib-2.0.so.0",
    "libgobject-2.0.so.0",
    "libnspr4.so",
    "libnss3.so",
    "libnssutil3.so",
    "libpango-1.0.so.0",
    "libsmime3.so",
    "libudev.so.1",
    "libxcb.so.1",
    "libxkbcommon.so.0",
  },
  f"{_CEF}/chrome-sandbox": set(),
  f"{_CEF}/laufey_headless_launch_test": {"libstdc++.so.6"},
  f"{_CEF}/liblaufey_headless_runtime.so": set(),
  f"{_CEF}/libvk_swiftshader.so": set(),
  f"{_CEF}/libvulkan.so.1": set(),
}

# readelf -d: " 0x0000000000000001 (NEEDED)  Shared library: [libc.so.6]"
_NEEDED = re.compile(r"\(NEEDED\)\s+Shared library: \[([^\]]+)\]")


def needed_libraries(readelf_d):
  """The sonames a `readelf -d` listing declares."""
  return sorted(set(_NEEDED.findall(readelf_d)))


def check_file(name, needed):
  """(problems, stale) for one file: `problems` fail the check, `stale` names
  listed libraries it no longer links."""
  allowed = EXPECTED.get(name)
  if allowed is None:
    return ([f"{name}: an ELF file with no entry in EXPECTED"], [])
  problems = [
    f"{name}: unexpected NEEDED {lib}"
    for lib in needed
    if lib not in allowed and lib not in CORE and not LOADER.match(lib)
  ]
  return (problems, sorted(allowed - set(needed)))


def is_elf(path):
  with open(path, "rb") as f:
    return f.read(4) == b"\x7fELF"


def elf_files(arg):
  """[(name, path)] for one argument: a file, or a directory walked."""
  if os.path.isfile(arg):
    return [(os.path.basename(arg), arg)]
  found = []
  for dirpath, _, names in os.walk(arg):
    for n in names:
      path = os.path.join(dirpath, n)
      if os.path.islink(path) or not os.path.isfile(path) or not is_elf(path):
        continue
      found.append((os.path.relpath(path, arg).replace(os.sep, "/"), path))
  return sorted(found)


def main(argv):
  if len(argv) < 2:
    print(__doc__.strip().splitlines()[0], file=sys.stderr)
    print("usage: linux_needed.py <directory or file>...", file=sys.stderr)
    return 2
  readelf = os.environ.get("READELF", "readelf")
  failed = False
  for arg in argv[1:]:
    files = elf_files(arg)
    if not files:
      print(f"{arg}: no ELF files")
      failed = True
    for name, path in files:
      out = subprocess.run(
        [readelf, "-d", "-W", path], check=True, capture_output=True, text=True
      ).stdout
      problems, stale = check_file(name, needed_libraries(out))
      for p in problems:
        print(f"ERROR: {p}")
        failed = True
      if stale:
        print(f"note: {name}: listed but not linked: {', '.join(stale)}")
  if failed:
    print(
      "Add a new dependency to linux_needed.py only with a way for packages "
      "to get it (denext's .deb / .rpm lists, or a package that pulls it in)."
    )
    return 1
  print("linux_needed: every NEEDED entry is expected")
  return 0


if __name__ == "__main__":
  sys.exit(main(sys.argv))
