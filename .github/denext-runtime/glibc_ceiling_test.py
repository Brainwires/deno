#!/usr/bin/env python3
"""Tests for glibc_ceiling.py, denext_runtime.yml's check that a Linux
release binary requires no glibc newer than GLIBC_CEILING: the version
references are read from `objdump -p`, versions compare numerically (2.2.5 <
2.17 < 2.27), and a binary over the ceiling fails with the symbols that pulled
the version in. The last cases run the script against a stand-in `objdump`.

Usage: python3 -I .github/denext-runtime/glibc_ceiling_test.py
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "glibc_ceiling.py")

spec = importlib.util.spec_from_file_location("glibc_ceiling", SCRIPT)
gc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gc)


def objdump_p(*libs):
  """`objdump -p` text whose Version References list `libs`:
  [(library, [version names])]."""
  lines = [
    "",
    "deno:     file format elf64-x86-64",
    "",
    "Dynamic Section:",
    "  NEEDED               libc.so.6",
    "",
    "Version References:",
  ]
  n = 2
  for lib, versions in libs:
    lines.append(f"  required from {lib}:")
    for v in versions:
      lines.append(f"    0x09691a75 0x00 {n:02d} {v}")
      n += 1
  lines.append("")
  return "\n".join(lines) + "\n"


# Upstream Deno 2.9.8's x86_64 `deno`, trimmed.
UPSTREAM = objdump_p(
  ("libdl.so.2", ["GLIBC_2.2.5"]),
  ("libgcc_s.so.1", ["GCC_3.0", "GCC_3.3", "GCC_4.2.0"]),
  ("libpthread.so.0", ["GLIBC_2.2.5", "GLIBC_2.3.2", "GLIBC_2.12"]),
  ("libm.so.6", ["GLIBC_2.2.5"]),
  ("libc.so.6", ["GLIBC_2.2.5", "GLIBC_2.3", "GLIBC_2.17", "GLIBC_2.25", "GLIBC_2.27"]),
)

# A build on ubuntu-22.04 without the sysroot.
JAMMY = objdump_p(
  ("libgcc_s.so.1", ["GCC_3.0"]),
  ("libm.so.6", ["GLIBC_2.2.5", "GLIBC_2.27", "GLIBC_2.29"]),
  ("libc.so.6", ["GLIBC_2.2.5", "GLIBC_2.28", "GLIBC_2.33", "GLIBC_2.34", "GLIBC_2.35"]),
)

JAMMY_T = """
deno:     file format elf64-x86-64

DYNAMIC SYMBOL TABLE:
0000000000000000      DF *UND*	0000000000000000 (GLIBC_2.2.5) malloc
0000000000000000      DF *UND*	0000000000000000 (GLIBC_2.34) pthread_create
0000000000000000      DF *UND*	0000000000000000  GLIBC_2.34  __libc_start_main
0000000000000000  w   DF *UND*	0000000000000000 (GLIBC_2.27) copy_file_range
0000000000000000      DF *UND*	0000000000000000 (GLIBC_2.35) __memcpy_chk
0000000000000000      DF *UND*	0000000000000000 (GLIBC_2.29) exp
00000000012345a0 g    DF .text	0000000000000010  Base        not_glibc
"""


class Parse(unittest.TestCase):
  def test_versions_compare_numerically(self):
    self.assertEqual(gc.parse_version("2.27"), (2, 27))
    self.assertEqual(gc.parse_version("GLIBC_2.2.5"), (2, 2, 5))
    self.assertLess(gc.parse_version("2.2.5"), gc.parse_version("2.17"))
    self.assertLess(gc.parse_version("2.17"), gc.parse_version("2.27"))
    self.assertLess(gc.parse_version("2.9"), gc.parse_version("2.10"))

  def test_non_versions(self):
    for text in ["", "2", "GLIBC_", "GLIBC_PRIVATE", "GCC_3.0", "2.x", "GLIBC_2.27 "]:
      self.assertIsNone(gc.parse_version(text), text)

  def test_version_references(self):
    refs = gc.version_references(UPSTREAM)
    self.assertIn(("libdl.so.2", "GLIBC_2.2.5"), refs)
    self.assertIn(("libgcc_s.so.1", "GCC_3.3"), refs)
    self.assertIn(("libc.so.6", "GLIBC_2.27"), refs)
    self.assertEqual(len(refs), 13)

  def test_stops_at_the_next_section(self):
    text = UPSTREAM + "Private flags = 0:\n    0x0 0x00 99 GLIBC_9.99\n"
    self.assertNotIn(("libc.so.6", "GLIBC_9.99"), gc.version_references(text))

  def test_newest_glibc_ignores_other_namespaces(self):
    self.assertEqual(gc.newest_glibc(gc.version_references(UPSTREAM)), ((2, 27), ["libc.so.6"]))
    refs = [("libgcc_s.so.1", "GCC_7.0.0"), ("libc.so.6", "GLIBC_PRIVATE"), ("libc.so.6", "GLIBC_2.17")]
    self.assertEqual(gc.newest_glibc(refs), ((2, 17), ["libc.so.6"]))

  def test_newest_glibc_names_every_library(self):
    refs = [("libm.so.6", "GLIBC_2.29"), ("libc.so.6", "GLIBC_2.29"), ("libc.so.6", "GLIBC_2.2.5")]
    self.assertEqual(gc.newest_glibc(refs), ((2, 29), ["libm.so.6", "libc.so.6"]))

  def test_no_glibc_at_all(self):
    self.assertIsNone(gc.newest_glibc([]))
    self.assertIsNone(gc.newest_glibc([("libgcc_s.so.1", "GCC_3.0")]))
    self.assertEqual(gc.version_references("deno: file format elf64-x86-64\n"), [])

  def test_symbols_over(self):
    self.assertEqual(
      gc.symbols_over(JAMMY_T, (2, 27)),
      ["exp@GLIBC_2.29", "__libc_start_main@GLIBC_2.34", "pthread_create@GLIBC_2.34", "__memcpy_chk@GLIBC_2.35"],
    )
    self.assertEqual(gc.symbols_over(JAMMY_T, (2, 35)), [])


class Check(unittest.TestCase):
  def test_at_the_ceiling(self):
    ok, lines = gc.check("deno", (2, 27), UPSTREAM)
    self.assertTrue(ok)
    self.assertIn("GLIBC_2.27 (from libc.so.6; ceiling GLIBC_2.27)", lines[0])

  def test_under_the_ceiling(self):
    ok, _ = gc.check("deno", (2, 28), UPSTREAM)
    self.assertTrue(ok)

  def test_over_the_ceiling(self):
    ok, lines = gc.check("deno", (2, 27), JAMMY, JAMMY_T)
    self.assertFalse(ok)
    self.assertTrue(lines[0].startswith("::error::deno requires GLIBC_2.35 (from libc.so.6)"))
    self.assertIn("    __memcpy_chk@GLIBC_2.35", lines)
    self.assertIn("    exp@GLIBC_2.29", lines)
    self.assertNotIn("    copy_file_range@GLIBC_2.27", lines)

  def test_one_minor_over(self):
    ok, _ = gc.check("deno", (2, 26), UPSTREAM)
    self.assertFalse(ok)

  def test_no_glibc_is_a_failure(self):
    ok, lines = gc.check("deno", (2, 27), objdump_p(("libgcc_s.so.1", ["GCC_3.0"])))
    self.assertFalse(ok)
    self.assertIn("requires no GLIBC_ symbol version", lines[0])


class Script(unittest.TestCase):
  """The script end to end, with a stand-in `objdump` on PATH that prints
  the text in <file>.p / <file>.t for `-p` / `-T`."""

  def setUp(self):
    self.tmp = tempfile.TemporaryDirectory()
    self.dir = self.tmp.name
    bin_dir = os.path.join(self.dir, "bin")
    os.mkdir(bin_dir)
    stub = os.path.join(bin_dir, "objdump")
    with open(stub, "w") as f:
      f.write(
        "#!/bin/sh\n"
        'case "$1" in -p) ext=p ;; -T) ext=t ;; *) exit 2 ;; esac\n'
        'test -f "$2.$ext" || { echo "objdump: $2: no such file" >&2; exit 1; }\n'
        'cat "$2.$ext"\n'
      )
    os.chmod(stub, 0o755)
    self.env = dict(os.environ, PATH=bin_dir + os.pathsep + os.environ.get("PATH", ""))

  def tearDown(self):
    self.tmp.cleanup()

  def binary(self, name, p, t=""):
    path = os.path.join(self.dir, name)
    with open(path + ".p", "w") as f:
      f.write(p)
    with open(path + ".t", "w") as f:
      f.write(t)
    return path

  def run_script(self, *args):
    return subprocess.run(
      [sys.executable, "-I", SCRIPT, *args], capture_output=True, text=True, env=self.env
    )

  def test_every_file_under_the_ceiling(self):
    deno = self.binary("deno", UPSTREAM)
    denort = self.binary("denort", UPSTREAM)
    r = self.run_script("2.27", deno, denort)
    self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
    self.assertEqual(r.stdout.count("ceiling GLIBC_2.27"), 2)

  def test_one_file_over_fails_and_names_the_symbols(self):
    deno = self.binary("deno", UPSTREAM)
    denort = self.binary("denort", JAMMY, JAMMY_T)
    r = self.run_script("2.27", deno, denort)
    self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
    self.assertIn(f"::error::{denort} requires GLIBC_2.35", r.stdout)
    self.assertIn("__libc_start_main@GLIBC_2.34", r.stdout)
    self.assertIn(f"{deno}: newest glibc version required GLIBC_2.27", r.stdout)

  def test_objdump_failure_fails(self):
    r = self.run_script("2.27", os.path.join(self.dir, "missing"))
    self.assertEqual(r.returncode, 1)
    self.assertIn("::error::objdump -p", r.stdout)

  def test_usage_errors(self):
    self.assertEqual(self.run_script().returncode, 2)
    self.assertEqual(self.run_script("2.27").returncode, 2)
    r = self.run_script("latest", self.binary("deno", UPSTREAM))
    self.assertEqual(r.returncode, 2)
    self.assertIn("not a glibc version", r.stderr)


if __name__ == "__main__":
  unittest.main()
