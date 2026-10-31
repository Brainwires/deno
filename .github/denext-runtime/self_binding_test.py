#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests for self_binding.py, denext_runtime.yml's check that the Linux
libdenort binds its own symbols to itself (-Bsymbolic, DENEXT.md item 187):
the SYMBOLIC flag is read from `readelf -d`, the defined symbols from `readelf --dyn-syms` and the relocated ones
from `readelf -r`; a library whose relocations still name its own symbols, or
that no longer exports a name the `--exports` list holds, fails with the
names. The last cases run the script against a stand-in `readelf`.

Usage: python3 -I .github/denext-runtime/self_binding_test.py
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "self_binding.py")

spec = importlib.util.spec_from_file_location("self_binding", SCRIPT)
sb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sb)

DYNAMIC_PLAIN = """
Dynamic section at offset 0x5d4a000 contains 30 entries:
  Tag        Type                         Name/Value
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x000000000000000e (SONAME)             Library soname: [libdenort.so]
 0x000000000000001e (FLAGS)              BIND_NOW
 0x000000006ffffffb (FLAGS_1)            Flags: NOW
"""

DYNAMIC_SYMBOLIC_FLAGS = DYNAMIC_PLAIN.replace(
  "(FLAGS)              BIND_NOW", "(FLAGS)              SYMBOLIC BIND_NOW"
)

DYNAMIC_DT_SYMBOLIC = DYNAMIC_PLAIN + (
  " 0x0000000000000010 (SYMBOLIC)           0x0\n"
)

DYNSYMS = """
Symbol table '.dynsym' contains 6 entries:
   Num:    Value          Size Type    Bind   Vis       Ndx Name
     0: 0000000000000000     0 NOTYPE  LOCAL  DEFAULT   UND
     1: 0000000000000000     0 FUNC    GLOBAL DEFAULT   UND malloc@GLIBC_2.2.5 (2)
     2: 0000000000000000     0 NOTYPE  WEAK   DEFAULT   UND __gmon_start__
     3: 000000000236f350  2400 FUNC    GLOBAL DEFAULT    14 BrotliDecoderDecompressStream
     4: 0000000001234560   120 FUNC    GLOBAL DEFAULT    14 napi_create_object
     5: 0000000001234600    64 FUNC    GLOBAL DEFAULT    14 laufey_runtime_init
"""

RELOCS_SELF = """
Relocation section '.rela.dyn' at offset 0xe588 contains 3 entries:
    Offset             Info             Type               Symbol's Value  Symbol's Name + Addend
0000000005e1e500  0000000000000008 R_X86_64_RELATIVE                    1a2b3c
0000000005e1e568  0000000300000006 R_X86_64_GLOB_DAT      000000000236f350 BrotliDecoderDecompressStream + 0
0000000005e1e570  0000000100000006 R_X86_64_GLOB_DAT      0000000000000000 malloc@GLIBC_2.2.5 + 0

Relocation section '.rela.plt' at offset 0xf000 contains 1 entry:
    Offset             Info             Type               Symbol's Value  Symbol's Name + Addend
0000000005e20000  0000000400000007 R_X86_64_JUMP_SLOT     0000000001234560 napi_create_object + 0
"""

RELOCS_CLEAN = """
Relocation section '.rela.dyn' at offset 0xe588 contains 2 entries:
    Offset             Info             Type               Symbol's Value  Symbol's Name + Addend
0000000005e1e500  0000000000000008 R_X86_64_RELATIVE                    1a2b3c
0000000005e1e570  0000000100000006 R_X86_64_GLOB_DAT      0000000000000000 malloc@GLIBC_2.2.5 + 0
"""


class ParseTest(unittest.TestCase):
  def test_defined_symbols_skip_undefined_and_local(self):
    self.assertEqual(
      sb.defined_symbols(DYNSYMS),
      {
        "BrotliDecoderDecompressStream",
        "napi_create_object",
        "laufey_runtime_init",
      },
    )

  def test_relocated_symbols_strip_versions(self):
    self.assertEqual(
      sb.relocated_symbols(RELOCS_SELF),
      {"BrotliDecoderDecompressStream", "malloc", "napi_create_object"},
    )

  def test_relative_relocations_name_no_symbol(self):
    self.assertEqual(sb.relocated_symbols(RELOCS_CLEAN), {"malloc"})

  def test_symbolic_flag(self):
    self.assertFalse(sb.is_symbolic(DYNAMIC_PLAIN))
    self.assertTrue(sb.is_symbolic(DYNAMIC_SYMBOLIC_FLAGS))
    self.assertTrue(sb.is_symbolic(DYNAMIC_DT_SYMBOLIC))

  def test_self_bound(self):
    self.assertEqual(
      sb.self_bound(DYNAMIC_PLAIN, DYNSYMS, RELOCS_SELF),
      (False, ["BrotliDecoderDecompressStream", "napi_create_object"]),
    )
    self.assertEqual(
      sb.self_bound(DYNAMIC_SYMBOLIC_FLAGS, DYNSYMS, RELOCS_CLEAN),
      (True, []),
    )


class ScriptTest(unittest.TestCase):
  """The script end to end, against a stand-in readelf."""

  def run_script(self, dynamic, relocs, exports=None):
    with tempfile.TemporaryDirectory() as d:
      files = {"-d": dynamic, "--dyn-syms": DYNSYMS, "-r": relocs}
      for flag, text in files.items():
        with open(os.path.join(d, flag.strip("-") + ".txt"), "w") as f:
          f.write(text)
      fake = os.path.join(d, "readelf")
      with open(fake, "w") as f:
        f.write(
          "#!/usr/bin/env python3\n"
          "import os, sys\n"
          f"d = {d!r}\n"
          "name = {'-d': 'd', '--dyn-syms': 'dyn-syms', '-r': 'r'}"
          "[sys.argv[1]]\n"
          "sys.stdout.write(open(os.path.join(d, name + '.txt')).read())\n"
        )
      os.chmod(fake, 0o755)
      env = dict(os.environ, READELF=fake)
      extra = []
      if exports is not None:
        listing = os.path.join(d, "exports.def")
        with open(listing, "w") as f:
          f.write("{ " + " ".join(f'"{n}";' for n in exports) + " };")
        extra = ["--exports", listing]
      return subprocess.run(
        [sys.executable, "-I", SCRIPT, *extra, "libdenort.so"],
        capture_output=True,
        text=True,
        env=env,
      )

  def test_passes_when_self_bound(self):
    r = self.run_script(DYNAMIC_SYMBOLIC_FLAGS, RELOCS_CLEAN)
    self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
    self.assertIn("ok", r.stdout)

  def test_fails_without_the_flag(self):
    r = self.run_script(DYNAMIC_PLAIN, RELOCS_CLEAN)
    self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
    self.assertIn("no SYMBOLIC flag", r.stdout)

  def test_listed_exports(self):
    r = self.run_script(
      DYNAMIC_DT_SYMBOLIC,
      RELOCS_CLEAN,
      ["napi_create_object", "laufey_runtime_init"],
    )
    self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
    r = self.run_script(
      DYNAMIC_DT_SYMBOLIC, RELOCS_CLEAN, ["napi_create_object", "uv_async_init"]
    )
    self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
    self.assertIn("1 listed export(s) missing", r.stdout)
    self.assertIn("uv_async_init", r.stdout)

  def test_export_list_parses_the_def_format(self):
    self.assertEqual(
      sb.export_list('{ "napi_wrap"; "uv_hrtime"; };'),
      {"napi_wrap", "uv_hrtime"},
    )

  def test_fails_and_names_the_symbols(self):
    r = self.run_script(DYNAMIC_PLAIN, RELOCS_SELF)
    self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
    self.assertIn("no SYMBOLIC flag", r.stdout)
    self.assertIn("BrotliDecoderDecompressStream", r.stdout)
    self.assertIn("napi_create_object", r.stdout)
    self.assertNotIn("  malloc", r.stdout)

  def test_usage(self):
    for args in ([], ["--exports"]):
      r = subprocess.run(
        [sys.executable, "-I", SCRIPT, *args], capture_output=True, text=True
      )
      self.assertEqual(r.returncode, 2)


if __name__ == "__main__":
  unittest.main()
