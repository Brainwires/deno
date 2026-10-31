#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Check that a Linux shared library binds its own symbols to itself.

A desktop backend (laufey) dlopen()s libdenort into a process that has
already loaded the system's libraries (WebKitGTK and libsoup bring
libbrotlidec, for one). rustc's cdylib version script exports every
`#[no_mangle]` function of every crate linked in (the brotli crate's C API,
temporal_rs, the napi_* functions, rusty_v8's callbacks), and a call from
libdenort to one of its own exported functions goes through a dynamic
relocation that the loader resolves in the global scope first. So
`node:zlib`'s Brotli called the system libbrotlidec's
`BrotliDecoderDecompressStream` with the Rust decoder's state and spun at
100% CPU. Linked with `-Bsymbolic` (cli/rt_desktop/build.rs), every such
reference binds inside the library at link time.

The check: the dynamic section carries the SYMBOLIC flag (`DT_SYMBOLIC`, or
`SYMBOLIC` in `DT_FLAGS`), and no dynamic relocation names a symbol the
library itself defines (`readelf -r` against `readelf --dyn-syms`). On a
failure it lists those symbols.

Usage: self_binding.py <file.so>...
Exits 0 when every file passes, 1 when one doesn't, 2 on a usage error.
READELF names the readelf to run (default: readelf). denext_runtime.yml's
libdenort job runs it for the Linux targets (DENEXT.md item 187).
"""

import os
import re
import subprocess
import sys

# readelf -r -W: "0000000005e1e568  0000026600000006 R_X86_64_GLOB_DAT
#   000000000236f350 BrotliDecoderDecompressStream + 0"
_RELOC = re.compile(
    r"^\s*[0-9a-fA-F]+\s+[0-9a-fA-F]+\s+(R_\S+)\s+[0-9a-fA-F]+\s+(\S+)"
)
# readelf --dyn-syms -W: "  1234: 000000000236f350  2400 FUNC GLOBAL DEFAULT
#   14 BrotliDecoderDecompressStream"
_DYNSYM = re.compile(
    r"^\s*\d+:\s+[0-9a-fA-F]+\s+\S+\s+(\S+)\s+(\S+)\s+\S+\s+(\S+)\s+(\S+)"
)


def strip_version(name):
  """'foo@GLIBC_2.2.5' / 'foo@@V1' / 'foo' -> 'foo'."""
  return name.split("@", 1)[0]


def defined_symbols(dynsyms_text):
  """The names of the symbols a `readelf --dyn-syms -W` listing defines."""
  out = set()
  for line in dynsyms_text.splitlines():
    m = _DYNSYM.match(line)
    if not m:
      continue
    sym_type, bind, ndx, name = m.groups()
    if ndx == "UND" or bind == "LOCAL" or sym_type == "SECTION":
      continue
    out.add(strip_version(name))
  return out


def relocated_symbols(relocs_text):
  """The symbol names the dynamic relocations of `readelf -r -W` name."""
  out = set()
  for line in relocs_text.splitlines():
    m = _RELOC.match(line)
    if m:
      out.add(strip_version(m.group(2)))
  return out


def is_symbolic(dynamic_text):
  """Whether a `readelf -d` listing has DT_SYMBOLIC or DF_SYMBOLIC."""
  for line in dynamic_text.splitlines():
    if "(SYMBOLIC)" in line:
      return True
    if "(FLAGS)" in line and re.search(r"\bSYMBOLIC\b", line):
      return True
  return False


def self_bound(dynamic_text, dynsyms_text, relocs_text):
  """(symbolic, the defined symbols a dynamic relocation still names)."""
  defined = defined_symbols(dynsyms_text)
  return (
    is_symbolic(dynamic_text),
    sorted(defined & relocated_symbols(relocs_text)),
  )


def _readelf(*args):
  readelf = os.environ.get("READELF", "readelf")
  return subprocess.run(
    [readelf, *args], check=True, capture_output=True, text=True
  ).stdout


def main(argv):
  if len(argv) < 2:
    print(__doc__.strip().splitlines()[0], file=sys.stderr)
    print("usage: self_binding.py <file.so>...", file=sys.stderr)
    return 2
  failed = False
  for path in argv[1:]:
    symbolic, leaks = self_bound(
      _readelf("-d", "-W", path),
      _readelf("--dyn-syms", "-W", path),
      _readelf("-r", "-W", path),
    )
    if not symbolic:
      print(f"{path}: not linked with -Bsymbolic (no SYMBOLIC flag)")
      failed = True
    if leaks:
      print(
        f"{path}: {len(leaks)} dynamic relocation(s) name a symbol the "
        "library defines, so another loaded library could take the call:"
      )
      for name in leaks[:50]:
        print(f"  {name}")
      if len(leaks) > 50:
        print(f"  ... and {len(leaks) - 50} more")
      failed = True
    if symbolic and not leaks:
      print(f"{path}: binds its own symbols to itself: ok")
  return 1 if failed else 0


if __name__ == "__main__":
  sys.exit(main(sys.argv))
