#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Record the toolchain a release build ran with, for BUILD_INFO.json (the
production audit's P9).

Every input a release build installs is pinned where its package manager can
pin it (rust-toolchain.toml's exact channel, STOCK_DENO_VERSION, choco's
`--version`, the WebView2 SDK's SHA-256, the sysroot's clang major); apt and
Homebrew install whatever their index holds that day. Either way the archive
says what it was built with: the runner image (`ImageOS` / `ImageVersion`),
rustc (with its commit and LLVM), cargo, the C compilers (`$CC`, clang, MSVC
through vswhere), Python, Deno and Node when present, and the versions of the
apt / Homebrew packages a job installed.

Usage: toolchain_info.py [--out FILE] [--require NAME]... [--tool NAME]...
                         [--apt PKG]... [--brew PKG]...
  --require  fail (exit 1) when that entry (rustc, cargo, cc, clang, msvc,
             python, deno, node, or a --tool) was not found
  --tool     also record `NAME --version` (e.g. cmake, ninja) under "tools"
  --apt      record the installed version of a Debian package (dpkg-query)
  --brew     record the installed version of a Homebrew formula
Writes one JSON object to FILE (or stdout). A tool that is not on PATH is
null, never an error, unless it is --require'd.
"""

import json
import os
import platform
import shutil
import subprocess
import sys


def run(argv):
  """(exit code, stdout + stderr) of `argv`, or None when it can't start."""
  try:
    p = subprocess.run(
      argv,
      stdin=subprocess.DEVNULL,
      stdout=subprocess.PIPE,
      stderr=subprocess.STDOUT,
      timeout=120,
      check=False,
    )
  except (OSError, subprocess.SubprocessError):
    return None
  return p.returncode, p.stdout.decode("utf-8", "replace")


def first_line(text):
  for line in text.splitlines():
    line = line.strip()
    if line:
      return line
  return None


def version_line(argv, runner=run, which=shutil.which):
  """The first line `argv` prints, when it exits 0; None otherwise."""
  if which(argv[0]) is None:
    return None
  r = runner(argv)
  if r is None or r[0] != 0:
    return None
  return first_line(r[1])


def parse_rustc_vv(text):
  """`rustc -vV` -> {version, commit, host, llvm}."""
  fields = {}
  for line in text.splitlines():
    if ": " in line:
      k, v = line.split(": ", 1)
      fields[k.strip()] = v.strip()
  return {
    "version": first_line(text),
    "release": fields.get("release"),
    "commit": fields.get("commit-hash"),
    "host": fields.get("host"),
    "llvm": fields.get("LLVM version"),
  }


def rustc_info(runner=run, which=shutil.which):
  if which("rustc") is None:
    return None
  r = runner(["rustc", "-vV"])
  if r is None or r[0] != 0:
    return None
  return parse_rustc_vv(r[1])


def cc_info(env, runner=run, which=shutil.which):
  """`$CC --version` (the sysroot's clang on Linux), else `cc --version`."""
  cc = env.get("CC") or "cc"
  line = version_line([cc, "--version"], runner, which)
  if line is None:
    return None
  return {"command": cc, "version": line}


def msvc_info(env, runner=run, exists=os.path.isfile):
  """The Visual Studio install cargo's cc crate and link.exe use (vswhere's
  newest), and the default MSVC tools version in it. None off Windows."""
  if platform.system() != "Windows" and not env.get("VSWHERE"):
    return None
  vswhere = env.get("VSWHERE") or os.path.join(
    env.get("ProgramFiles(x86)", r"C:\Program Files (x86)"),
    "Microsoft Visual Studio",
    "Installer",
    "vswhere.exe",
  )
  if not exists(vswhere):
    return None
  info = {}
  for key, prop in (
    ("product", "displayName"),
    ("version", "catalog_productDisplayVersion"),
    ("installationVersion", "installationVersion"),
    ("path", "installationPath"),
  ):
    r = runner([vswhere, "-latest", "-products", "*", "-property", prop])
    info[key] = first_line(r[1]) if r is not None and r[0] == 0 else None
  tools = None
  if info.get("path"):
    default = os.path.join(
      info["path"], "VC", "Auxiliary", "Build", "Microsoft.VCToolsVersion.default.txt"
    )
    if exists(default):
      try:
        with open(default, encoding="utf-8") as f:
          tools = first_line(f.read())
      except OSError:
        tools = None
  # msvc-dev-cmd's environment, when a job set one up (laufey, package).
  info["vcTools"] = env.get("VCToolsVersion") or tools
  info["windowsSdk"] = env.get("WindowsSDKVersion", "").rstrip("\\") or None
  del info["path"]
  return info


def apt_versions(pkgs, runner=run, which=shutil.which):
  out = {}
  for pkg in pkgs:
    v = None
    if which("dpkg-query") is not None:
      r = runner(["dpkg-query", "-W", "-f=${Version}", pkg])
      if r is not None and r[0] == 0:
        v = first_line(r[1])
    out[pkg] = v
  return out


def brew_versions(pkgs, runner=run, which=shutil.which):
  out = {}
  for pkg in pkgs:
    v = None
    if which("brew") is not None:
      r = runner(["brew", "list", "--versions", pkg])
      line = first_line(r[1]) if r is not None and r[0] == 0 else None
      # "llvm 21.1.5" (several installed versions: "ninja 1.12.1 1.13.2").
      if line and line.split()[0] == pkg:
        v = " ".join(line.split()[1:]) or None
    out[pkg] = v
  return out


def collect(env, tools=(), apt=(), brew=(), runner=run, which=shutil.which):
  info = {
    "runner": {
      # The hosted runner image (actions/runner-images), e.g. ubuntu24 /
      # 20261026.1; null on a self-hosted runner.
      "image": env.get("ImageOS") or None,
      "imageVersion": env.get("ImageVersion") or None,
      "os": env.get("RUNNER_OS") or platform.system(),
      "arch": env.get("RUNNER_ARCH") or platform.machine(),
    },
    "rustc": rustc_info(runner, which),
    "cargo": version_line(["cargo", "-V"], runner, which),
    "rustup": version_line(["rustup", "-V"], runner, which),
    "cc": cc_info(env, runner, which),
    "clang": version_line(["clang", "--version"], runner, which),
    "msvc": msvc_info(env, runner),
    "python": f"{platform.python_implementation()} {platform.python_version()}",
    "deno": version_line(["deno", "--version"], runner, which),
    "node": version_line(["node", "--version"], runner, which),
  }
  if tools:
    info["tools"] = {t: version_line([t, "--version"], runner, which) for t in tools}
  packages = {}
  if apt:
    packages["apt"] = apt_versions(apt, runner, which)
  if brew:
    packages["brew"] = brew_versions(brew, runner, which)
  if packages:
    info["packages"] = packages
  return info


def missing(info, required):
  tools = info.get("tools") or {}
  return [n for n in required if info.get(n, tools.get(n)) is None]


def parse_args(argv):
  opts = {"out": None, "require": [], "tool": [], "apt": [], "brew": []}
  i = 0
  while i < len(argv):
    flag = argv[i]
    key = flag[2:] if flag.startswith("--") else None
    if key not in opts or i + 1 >= len(argv):
      raise SystemExit(__doc__)
    if key == "out":
      opts["out"] = argv[i + 1]
    else:
      opts[key].append(argv[i + 1])
    i += 2
  return opts


def main(argv):
  opts = parse_args(argv)
  info = collect(os.environ, opts["tool"], opts["apt"], opts["brew"])
  text = json.dumps(info, indent=2, sort_keys=False) + "\n"
  if opts["out"]:
    with open(opts["out"], "w", encoding="utf-8") as f:
      f.write(text)
  print(text, end="")
  absent = missing(info, opts["require"])
  if absent:
    print(f"::error::toolchain_info.py: not found: {', '.join(absent)}", file=sys.stderr)
    return 1
  return 0


if __name__ == "__main__":
  sys.exit(main(sys.argv[1:]))
