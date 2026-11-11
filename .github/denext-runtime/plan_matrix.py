#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""The release matrix of denext_runtime.yml (its setup job's `plan` step).

Prints the JSON list of targets on stdout. Each entry has:

- `target`: the Rust target triple.
- `os`: the GitHub image. It names the caches and the artifacts, whatever
  runs the job.
- `runner`: where the build jobs run (denort, denort bin, cli). For a macOS
  target this is the self-hosted pool its repository variable names
  (RUNNER_MACOS_ARM / RUNNER_MACOS_X64: a bare label or a JSON array), else
  the image.
- `gui_runner`: where the jobs that launch the packaged app run (package and
  e2e_fork, whose smoke and e2e open windows). Launching an app needs a GUI
  login session for the job's user. The mac-arm pool has one: its Tart guests
  log `runner` in at the console. The mac-x64 pool has none: the MacBook runs
  its jobs as `runner` over ssh + sudo inside `sandbox-exec`. There AppKit
  aborts the app (`xpc_connection_set_target_uid` API misuse, SIGILL; or
  SIGSEGV in SLSNewConnection), and the sandbox refuses to exec the setuid
  /bin/ps the e2e runner lists processes with (EPERM). That was the denext.14
  release failure. So x86_64-apple-darwin launches on GitHub's image unless
  RUNNER_MACOS_X64_GUI names a pool that has a GUI session. The arm64 GUI
  runner follows RUNNER_MACOS_ARM unless RUNNER_MACOS_ARM_GUI overrides it.
- `laufey_runner`: where the laufey job builds the backend hosts. For
  x86_64-unknown-linux-gnu this is the Ubuntu 22.04 Linux pool
  (RUNNER_LINUX_X64_2204), whose image carries every package that job
  installs, so it downloads none; the other targets stay on the image (the
  macOS leg `brew install`s llvm, the Windows legs use Chocolatey, which the
  pools' images lack, and Linux arm64 has no pool).

Env: TARGETS ("all" or a comma-separated list of triples),
     RUNNER_MACOS_ARM, RUNNER_MACOS_X64, RUNNER_MACOS_ARM_GUI,
     RUNNER_MACOS_X64_GUI, RUNNER_LINUX_X64_2204 (each optional).
"""
import json
import os
import sys

ALL_TARGETS = [
    {"target": "aarch64-apple-darwin", "os": "macos-15"},
    {"target": "x86_64-apple-darwin", "os": "macos-15-intel"},
    {"target": "x86_64-unknown-linux-gnu", "os": "ubuntu-22.04"},
    {"target": "aarch64-unknown-linux-gnu", "os": "ubuntu-22.04-arm"},
    {"target": "x86_64-pc-windows-msvc", "os": "windows-2025"},
    {"target": "aarch64-pc-windows-msvc", "os": "windows-11-arm"},
]


def _runner(value, image):
    value = (value or "").strip()
    if value.startswith("["):
        return json.loads(value)
    return value or image


def plan(env):
    """The matrix for `env` (a mapping like os.environ)."""
    build = {
        "macos-15": env.get("RUNNER_MACOS_ARM", ""),
        "macos-15-intel": env.get("RUNNER_MACOS_X64", ""),
    }
    gui = {
        # The mac-arm pool's guests have a console session: its GUI jobs
        # follow its build jobs.
        "macos-15": env.get("RUNNER_MACOS_ARM_GUI", "")
        or env.get("RUNNER_MACOS_ARM", ""),
        # The mac-x64 pool has no GUI session (see the module docstring):
        # never RUNNER_MACOS_X64 here.
        "macos-15-intel": env.get("RUNNER_MACOS_X64_GUI", ""),
    }
    laufey = {
        "x86_64-unknown-linux-gnu": env.get("RUNNER_LINUX_X64_2204", ""),
    }
    targets = []
    for t in ALL_TARGETS:
        t = dict(t)
        t["runner"] = _runner(build.get(t["os"], ""), t["os"])
        t["gui_runner"] = _runner(gui.get(t["os"], ""), t["os"])
        t["laufey_runner"] = _runner(laufey.get(t["target"], ""), t["os"])
        targets.append(t)
    want = [t.strip() for t in env.get("TARGETS", "all").split(",") if t.strip()]
    if want != ["all"]:
        known = {t["target"] for t in targets}
        unknown = [t for t in want if t not in known]
        if unknown:
            raise SystemExit(f"unknown targets: {unknown}")
        targets = [t for t in targets if t["target"] in want]
    return targets


if __name__ == "__main__":
    json.dump(plan(os.environ), sys.stdout)
    sys.stdout.write("\n")
