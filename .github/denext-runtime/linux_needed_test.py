#!/usr/bin/env python3
"""Tests for linux_needed.py, verify.sh's check of the libraries shipped Linux
ELF files declare: NEEDED entries are read from `readelf -d`, a library or an
ELF file not in the table fails, and the table's classes agree with each
other. The last cases run the script against a stand-in `readelf`.

Usage: python3 -I .github/denext-runtime/linux_needed_test.py
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "linux_needed.py")

spec = importlib.util.spec_from_file_location("linux_needed", SCRIPT)
ln = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ln)

LISTING = """
Dynamic section at offset 0x2d58 contains 4 entries:
  Tag        Type                         Name/Value
 0x0000000000000001 (NEEDED)             Shared library: [libz.so.1]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x000000000000000e (SONAME)             Library soname: [libdenort.so]
"""


def backends_of(name):
  if name.startswith(ln._WEBVIEW):
    return ["webview"]
  if name.startswith(ln._CEF):
    return ["cef"]
  return ["webview", "cef"]


class TableTest(unittest.TestCase):
  def test_every_library_has_a_way_into_a_package(self):
    for name, libs in ln.EXPECTED.items():
      for backend in backends_of(name):
        for lib in libs:
          self.assertTrue(
            lib in ln.PACKAGE_DEPS[backend]
            or lib in ln.TRANSITIVE[backend]
            or lib in ln.BUNDLED,
            f"{name} ({backend}): {lib}",
          )

  def test_declared_is_linked_or_explained(self):
    for backend, declared in ln.PACKAGE_DEPS.items():
      linked = set()
      for name, libs in ln.EXPECTED.items():
        if backend in backends_of(name):
          linked |= libs
      self.assertEqual(declared - linked, ln.DECLARED_NOT_NEEDED[backend])

  def test_toolchain_binaries_link_only_core(self):
    self.assertEqual(ln.EXPECTED["deno"], set())
    self.assertEqual(ln.EXPECTED["denort"], set())


class CheckTest(unittest.TestCase):
  def test_parse(self):
    self.assertEqual(ln.needed_libraries(LISTING), ["libc.so.6", "libz.so.1"])

  def test_expected(self):
    self.assertEqual(
      ln.check_file(
        "libdenort.so", ["ld-linux-aarch64.so.1", "libc.so.6", "libz.so.1"]
      ),
      ([], []),
    )

  def test_unexpected(self):
    problems, _ = ln.check_file("libdenort.so", ["libz.so.1", "libfoo.so.1"])
    self.assertEqual(problems, ["libdenort.so: unexpected NEEDED libfoo.so.1"])

  def test_a_library_of_another_file_is_not_allowed(self):
    problems, _ = ln.check_file("denort", ["libz.so.1"])
    self.assertEqual(problems, ["denort: unexpected NEEDED libz.so.1"])

  def test_unknown_file(self):
    problems, _ = ln.check_file("laufey/new_helper", ["libc.so.6"])
    self.assertEqual(
      problems, ["laufey/new_helper: an ELF file with no entry in EXPECTED"]
    )

  def test_stale_is_reported_not_failed(self):
    self.assertEqual(ln.check_file("libdenort.so", []), ([], ["libz.so.1"]))


class ScriptTest(unittest.TestCase):
  """The script end to end, against a stand-in readelf."""

  def run_script(self, listing, name="libdenort.so"):
    with tempfile.TemporaryDirectory() as d:
      fake = os.path.join(d, "readelf")
      text = os.path.join(d, "listing.txt")
      with open(text, "w") as f:
        f.write(listing)
      with open(fake, "w") as f:
        f.write(f"#!/bin/sh\ncat {text!r}\n")
      os.chmod(fake, 0o755)
      root = os.path.join(d, "root")
      os.mkdir(root)
      with open(os.path.join(root, name), "wb") as f:
        f.write(b"\x7fELF")
      with open(os.path.join(root, "README"), "w") as f:
        f.write("not ELF")
      return subprocess.run(
        [sys.executable, "-I", SCRIPT, root],
        env=dict(os.environ, READELF=fake),
        capture_output=True,
        text=True,
      )

  def test_pass(self):
    r = self.run_script(LISTING)
    self.assertEqual(r.returncode, 0, r.stdout)

  def test_fail_names_the_library(self):
    r = self.run_script(LISTING.replace("libz.so.1", "libfoo.so.1"))
    self.assertEqual(r.returncode, 1)
    self.assertIn("libdenort.so: unexpected NEEDED libfoo.so.1", r.stdout)

  def test_usage(self):
    r = subprocess.run(
      [sys.executable, "-I", SCRIPT], capture_output=True, text=True
    )
    self.assertEqual(r.returncode, 2)


if __name__ == "__main__":
  unittest.main()
