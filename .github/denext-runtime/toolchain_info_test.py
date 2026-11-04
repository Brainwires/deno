#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests for toolchain_info.py, the toolchain record denext_runtime.yml's
build jobs put in BUILD_INFO.json: rustc's `-vV` is parsed into its release,
commit and LLVM, an absent tool is null (never an error unless --require'd),
the runner image comes from ImageOS / ImageVersion, apt / Homebrew versions
are read from dpkg-query / `brew list --versions`, and the script's own JSON
output parses. The last case runs the script.

Usage: python3 -I .github/denext-runtime/toolchain_info_test.py
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "toolchain_info.py")

spec = importlib.util.spec_from_file_location("toolchain_info", SCRIPT)
ti = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ti)

RUSTC_VV = """rustc 1.99.1 (58cf8f9c3 2026-10-13)
binary: rustc
commit-hash: 58cf8f9c35b355199727627d07ae34a4e2846d67
commit-date: 2026-10-13
host: x86_64-unknown-linux-gnu
release: 1.99.1
LLVM version: 23.1.1
"""


def fake(outputs):
  """A runner answering `outputs` ({argv tuple: (code, text)}) and a `which`
  that finds exactly the programs named in it."""
  progs = {argv[0] for argv in outputs}

  def runner(argv):
    return outputs.get(tuple(argv))

  def which(name):
    return f"/usr/bin/{name}" if name in progs else None

  return runner, which


class ToolchainInfoTest(unittest.TestCase):
  def test_rustc_vv(self):
    info = ti.parse_rustc_vv(RUSTC_VV)
    self.assertEqual(info["release"], "1.99.1")
    self.assertEqual(info["commit"], "58cf8f9c35b355199727627d07ae34a4e2846d67")
    self.assertEqual(info["host"], "x86_64-unknown-linux-gnu")
    self.assertEqual(info["llvm"], "23.1.1")
    self.assertEqual(info["version"], "rustc 1.99.1 (58cf8f9c3 2026-10-13)")

  def test_collect_linux(self):
    runner, which = fake({
      ("rustc", "-vV"): (0, RUSTC_VV),
      ("cargo", "-V"): (0, "cargo 1.99.1 (5f94df478 2026-08-27)\n"),
      ("clang-18", "--version"): (0, "Ubuntu clang version 18.1.3 (1ubuntu1)\nTarget: x86_64\n"),
      ("cmake", "--version"): (0, "cmake version 3.31.6\n\nCMake suite\n"),
      ("dpkg-query", "-W", "-f=${Version}", "cmake"): (0, "3.28.3-1build7"),
      ("dpkg-query", "-W", "-f=${Version}", "nope"): (1, "dpkg-query: no packages found\n"),
    })
    env = {"ImageOS": "ubuntu24", "ImageVersion": "20261026.1", "RUNNER_OS": "Linux",
           "RUNNER_ARCH": "X64", "CC": "clang-18"}
    info = ti.collect(env, tools=["cmake"], apt=["cmake", "nope"], runner=runner, which=which)
    self.assertEqual(info["runner"], {"image": "ubuntu24", "imageVersion": "20261026.1",
                                      "os": "Linux", "arch": "X64"})
    self.assertEqual(info["rustc"]["release"], "1.99.1")
    self.assertEqual(info["cargo"], "cargo 1.99.1 (5f94df478 2026-08-27)")
    self.assertEqual(info["cc"], {"command": "clang-18",
                                  "version": "Ubuntu clang version 18.1.3 (1ubuntu1)"})
    # Not on PATH: null, not an error.
    self.assertIsNone(info["clang"])
    self.assertIsNone(info["deno"])
    self.assertIsNone(info["node"])
    self.assertEqual(info["tools"], {"cmake": "cmake version 3.31.6"})
    self.assertEqual(info["packages"], {"apt": {"cmake": "3.28.3-1build7", "nope": None}})
    self.assertNotIn("brew", info["packages"])
    self.assertEqual(ti.missing(info, ["rustc", "cargo", "cmake"]), [])
    self.assertEqual(ti.missing(info, ["rustc", "deno", "node"]), ["deno", "node"])

  def test_failing_tool_is_null(self):
    runner, which = fake({("rustc", "-vV"): (1, "error: toolchain not installed\n"),
                          ("cargo", "-V"): (1, "error\n")})
    info = ti.collect({}, runner=runner, which=which)
    self.assertIsNone(info["rustc"])
    self.assertIsNone(info["cargo"])
    self.assertIsNone(info["runner"]["image"])
    self.assertNotIn("tools", info)
    self.assertNotIn("packages", info)
    self.assertEqual(ti.missing(info, ["rustc"]), ["rustc"])

  def test_brew(self):
    runner, which = fake({
      ("brew", "list", "--versions", "llvm"): (0, "llvm 21.1.5\n"),
      ("brew", "list", "--versions", "ninja"): (0, "ninja 1.12.1 1.13.2\n"),
      ("brew", "list", "--versions", "gone"): (1, ""),
    })
    self.assertEqual(ti.brew_versions(["llvm", "ninja", "gone"], runner, which),
                     {"llvm": "21.1.5", "ninja": "1.12.1 1.13.2", "gone": None})

  def test_msvc(self):
    with tempfile.TemporaryDirectory() as d:
      vswhere = os.path.join(d, "vswhere.exe")
      open(vswhere, "w").close()
      vs = os.path.join(d, "VS")
      build = os.path.join(vs, "VC", "Auxiliary", "Build")
      os.makedirs(build)
      with open(os.path.join(build, "Microsoft.VCToolsVersion.default.txt"), "w") as f:
        f.write("14.44.35207\n")
      props = {"displayName": "Visual Studio Enterprise 2022",
               "catalog_productDisplayVersion": "17.14.18",
               "installationVersion": "17.14.36616.10", "installationPath": vs}

      def runner(argv):
        return (0, props[argv[-1]] + "\n")

      env = {"VSWHERE": vswhere, "WindowsSDKVersion": "10.0.26100.0\\"}
      info = ti.msvc_info(env, runner)
      self.assertEqual(info, {"product": "Visual Studio Enterprise 2022", "version": "17.14.18",
                              "installationVersion": "17.14.36616.10",
                              "vcTools": "14.44.35207", "windowsSdk": "10.0.26100.0"})
      # msvc-dev-cmd's VCToolsVersion wins over the default file.
      env["VCToolsVersion"] = "14.43.34808"
      self.assertEqual(ti.msvc_info(env, runner)["vcTools"], "14.43.34808")

  def test_no_msvc_off_windows(self):
    if sys.platform == "win32":
      self.skipTest("Windows")
    self.assertIsNone(ti.msvc_info({}, lambda argv: None))

  def test_bad_args(self):
    with self.assertRaises(SystemExit):
      ti.parse_args(["--bogus", "x"])
    with self.assertRaises(SystemExit):
      ti.parse_args(["--tool"])
    self.assertEqual(ti.parse_args(["--out", "f", "--tool", "a", "--tool", "b"])["tool"], ["a", "b"])

  def test_script(self):
    with tempfile.TemporaryDirectory() as d:
      out = os.path.join(d, "toolchain.json")
      env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
      env.update({"ImageOS": "macos15", "ImageVersion": "20261027.0"})
      p = subprocess.run([sys.executable, "-I", SCRIPT, "--out", out, "--require", "python"],
                         stdin=subprocess.DEVNULL, capture_output=True, text=True, env=env,
                         check=False)
      self.assertEqual(p.returncode, 0, p.stderr)
      with open(out) as f:
        info = json.load(f)
      self.assertEqual(info, json.loads(p.stdout))
      self.assertEqual(info["runner"]["image"], "macos15")
      self.assertEqual(info["runner"]["imageVersion"], "20261027.0")
      self.assertTrue(info["python"].startswith(("CPython ", "PyPy ")))
      p = subprocess.run([sys.executable, "-I", SCRIPT, "--require", "no-such-tool-xyz"],
                         stdin=subprocess.DEVNULL, capture_output=True, text=True, env=env,
                         check=False)
      self.assertEqual(p.returncode, 1)
      self.assertIn("not found: no-such-tool-xyz", p.stderr)


if __name__ == "__main__":
  unittest.main()
