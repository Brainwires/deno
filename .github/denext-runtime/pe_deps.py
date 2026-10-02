# Copyright 2018-2026 the Deno authors. MIT license.
"""Check a Windows PE file: its machine type, and that every DLL it imports
(the import table; delay-loaded DLLs are optional by design and only listed)
resolves next to it or to the system.

Usage: pe_deps.py <expected machine: x64|arm64> <file>...
Prints the evidence; exits 1 when a file is not a PE image of the expected
machine or imports a DLL that resolves nowhere.
"""

import os
import struct
import sys

MACHINES = {"x64": 0x8664, "arm64": 0xAA64}


class PeError(Exception):
  pass


def _rva_to_offset(sections, rva):
  for va, vsize, raw, rawsize in sections:
    if va <= rva < va + max(vsize, rawsize):
      return raw + (rva - va)
  raise PeError(f"RVA {rva:#x} is in no section")


def _cstr(data, offset):
  end = data.index(b"\0", offset)
  return data[offset:end].decode("ascii", "replace")


def parse(data):
  """(machine, [imported DLLs], [delay-loaded DLLs]) of a PE image."""
  if data[:2] != b"MZ":
    raise PeError("no MZ header")
  pe = struct.unpack_from("<I", data, 0x3C)[0]
  if data[pe:pe + 4] != b"PE\0\0":
    raise PeError("no PE signature")
  machine, nsections, _, _, _, opt_size, _ = struct.unpack_from(
    "<HHIIIHH", data, pe + 4)
  opt = pe + 24
  magic = struct.unpack_from("<H", data, opt)[0]
  if magic == 0x20B:  # PE32+
    dirs = opt + 112
  elif magic == 0x10B:  # PE32
    dirs = opt + 96
  else:
    raise PeError(f"unknown optional header magic {magic:#x}")
  ndirs = struct.unpack_from("<I", data, dirs - 4)[0]
  sections = []
  sec = opt + opt_size
  for i in range(nsections):
    vsize, va, rawsize, raw = struct.unpack_from("<IIII", data,
                                                 sec + i * 40 + 8)
    sections.append((va, vsize, raw, rawsize))

  def directory(index):
    if index >= ndirs:
      return 0
    return struct.unpack_from("<I", data, dirs + index * 8)[0]

  imports = []
  rva = directory(1)
  if rva:
    off = _rva_to_offset(sections, rva)
    while True:
      desc = struct.unpack_from("<IIIII", data, off)
      if desc == (0, 0, 0, 0, 0):
        break
      imports.append(_cstr(data, _rva_to_offset(sections, desc[3])))
      off += 20
  delayed = []
  rva = directory(13)
  if rva:
    off = _rva_to_offset(sections, rva)
    while True:
      desc = struct.unpack_from("<IIIIIIII", data, off)
      if desc[1] == 0:
        break
      delayed.append(_cstr(data, _rva_to_offset(sections, desc[1])))
      off += 32
  return machine, imports, delayed


def _system_dirs():
  root = os.environ.get("SystemRoot") or os.environ.get("SYSTEMROOT")
  if not root:
    return []
  return [os.path.join(root, "System32"), root]


def resolves(dll, here, system_dirs):
  name = dll.lower()
  # API set contracts resolve inside the loader, not to a file.
  if name.startswith(("api-ms-win-", "ext-ms-")):
    return "api set"
  for d in [here] + system_dirs:
    try:
      if name in (n.lower() for n in os.listdir(d)):
        return os.path.join(d, dll)
    except OSError:
      pass
  return None


def main(argv):
  want = MACHINES[argv[1]]
  system_dirs = _system_dirs()
  failed = False
  for path in argv[2:]:
    with open(path, "rb") as f:
      data = f.read()
    try:
      machine, imports, delayed = parse(data)
    except (PeError, struct.error, ValueError) as e:
      print(f"   ERROR: {path}: not a PE image ({e})", file=sys.stderr)
      failed = True
      continue
    if machine != want:
      print(f"   ERROR: {path}: machine {machine:#x}, expected {want:#x}",
            file=sys.stderr)
      failed = True
    here = os.path.dirname(os.path.abspath(path))
    for dll in imports:
      where = resolves(dll, here, system_dirs)
      print(f"   dep: {dll} -> {where or 'UNRESOLVED'}")
      if not where:
        print(f"   ERROR: {path}: imports {dll}, which resolves nowhere",
              file=sys.stderr)
        failed = True
    for dll in delayed:
      print(f"   delay-loaded: {dll}")
  return 1 if failed else 0


if __name__ == "__main__":
  sys.exit(main(sys.argv))
