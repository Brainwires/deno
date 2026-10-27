#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Check that Linux ELF files require no glibc newer than a ceiling.

Reads each file's version references (`objdump -p`, the `.gnu.version_r`
section the dynamic loader checks), takes the newest `GLIBC_x.y[.z]` version
it requires from any library (libc, libm, libpthread, ld-linux, ...), and
fails when that is newer than the ceiling. On a failure it lists the symbols
bound to the offending versions (`objdump -T`), so the log names what pulled
them in.

Usage: glibc_ceiling.py <ceiling, e.g. 2.27> <file>...
Exits 0 when every file is at or under the ceiling, 1 when one is over it or
requires no GLIBC_ version at all (not a dynamically linked glibc binary),
2 on a usage error. denext_runtime.yml's cli job runs it in "Check binary"
with GLIBC_CEILING (DENEXT.md item 134).
"""

import re
import subprocess
import sys

_VERSION = re.compile(r"^GLIBC_(\d+(?:\.\d+)+)$")
_REQUIRED_FROM = re.compile(r"^\s*required from (\S+):\s*$")
# objdump -p: "    0x09691a75 0x00 05 GLIBC_2.2.5"
_VERNEED = re.compile(r"^\s*0x[0-9a-fA-F]+\s+0x[0-9a-fA-F]+\s+\d+\s+(\S+)\s*$")
# objdump -T: "... DF *UND*  0000000000000000 (GLIBC_2.27) copy_file_range"
# (binutils >= 2.35 parenthesizes the version of an undefined symbol).
_DYNSYM = re.compile(r"\*UND\*\s+[0-9a-fA-F]+\s+\(?(GLIBC_[0-9.]+)\)?\s+(\S+)")


def parse_version(text):
  """'2.27' / 'GLIBC_2.27' -> (2, 27); None for anything else."""
  if not text.startswith("GLIBC_"):
    text = "GLIBC_" + text
  m = _VERSION.match(text)
  if not m:
    return None
  return tuple(int(p) for p in m.group(1).split("."))


def format_version(version):
  return ".".join(str(p) for p in version)


def version_references(objdump_p):
  """`objdump -p` output -> [(library, version name)] from its
  "Version References" section."""
  refs = []
  lib = None
  in_section = False
  for line in objdump_p.splitlines():
    if line.startswith("Version References:"):
      in_section = True
      continue
    if not in_section:
      continue
    if line and not line[0].isspace():
      # The next top-level section.
      break
    m = _REQUIRED_FROM.match(line)
    if m:
      lib = m.group(1)
      continue
    m = _VERNEED.match(line)
    if m and lib is not None:
      refs.append((lib, m.group(1)))
  return refs


def newest_glibc(refs):
  """The newest GLIBC_ version among the references, with the libraries it is
  required from: ((2, 27), ['libc.so.6']), or None when there is none."""
  newest = None
  libs = []
  for lib, name in refs:
    version = parse_version(name)
    if version is None:
      continue
    if newest is None or version > newest:
      newest, libs = version, [lib]
    elif version == newest and lib not in libs:
      libs.append(lib)
  return None if newest is None else (newest, libs)


def symbols_over(objdump_t, ceiling):
  """`objdump -T` output -> sorted 'name@GLIBC_x.y' for each undefined symbol
  bound to a GLIBC_ version newer than the ceiling."""
  over = set()
  for line in objdump_t.splitlines():
    m = _DYNSYM.search(line)
    if not m:
      continue
    version = parse_version(m.group(1))
    if version is not None and version > ceiling:
      over.add(f"{m.group(2)}@{m.group(1)}")
  return sorted(over, key=lambda s: (parse_version(s.split("@")[1]), s))


def check(path, ceiling, objdump_p, objdump_t=None):
  """(ok, message lines) for one file."""
  found = newest_glibc(version_references(objdump_p))
  if found is None:
    return False, [
      f"::error::{path} requires no GLIBC_ symbol version (objdump -p lists none): not a dynamically linked glibc binary"
    ]
  newest, libs = found
  ceiling_text = format_version(ceiling)
  if newest <= ceiling:
    return True, [
      f"{path}: newest glibc version required GLIBC_{format_version(newest)} (from {', '.join(libs)}; ceiling GLIBC_{ceiling_text})"
    ]
  lines = [
    f"::error::{path} requires GLIBC_{format_version(newest)} (from {', '.join(libs)}), newer than the ceiling GLIBC_{ceiling_text}"
  ]
  if objdump_t is not None:
    over = symbols_over(objdump_t, ceiling)
    if over:
      lines.append("  symbols over the ceiling:")
      lines.extend(f"    {s}" for s in over[:50])
      if len(over) > 50:
        lines.append(f"    ... and {len(over) - 50} more")
  return False, lines


def _objdump(flag, path):
  return subprocess.run(
    ["objdump", flag, path], check=True, capture_output=True, text=True
  ).stdout


def main(argv):
  if len(argv) < 3:
    print("usage: glibc_ceiling.py <ceiling> <file>...", file=sys.stderr)
    return 2
  ceiling = parse_version(argv[1])
  if ceiling is None:
    print(f"glibc_ceiling.py: '{argv[1]}' is not a glibc version (want e.g. 2.27)", file=sys.stderr)
    return 2
  ok = True
  for path in argv[2:]:
    try:
      objdump_p = _objdump("-p", path)
    except (OSError, subprocess.CalledProcessError) as e:
      print(f"::error::objdump -p {path} failed: {e}")
      ok = False
      continue
    good, lines = check(path, ceiling, objdump_p)
    if not good and newest_glibc(version_references(objdump_p)) is not None:
      good, lines = check(path, ceiling, objdump_p, _objdump("-T", path))
    print("\n".join(lines))
    ok = ok and good
  return 0 if ok else 1


if __name__ == "__main__":
  sys.exit(main(sys.argv))
