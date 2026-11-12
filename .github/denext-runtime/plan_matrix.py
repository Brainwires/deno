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
- `rosetta`: x86_64-apple-darwin's build jobs (denort, denort bin, cli) run
  on the arm64 pool with the x86_64 Rust toolchain: cargo, rustc and every
  build script run as x86_64 under Rosetta 2, so the build is a native x86_64
  build (target/release, the x86_64 prebuilt V8, Apple's ld, the SDK the
  pool's image selects; signing is the same; rosetta_tools.sh runs the
  Command Line Tools' shims as arm64 below the x86_64 cargo). Not a cross build (`--target`):
  the snapshots are made in build scripts by the host's V8, and an arm64 V8's
  snapshot crashes the x86_64 binary on its first script (a
  ReadOnlyHeapObjectIterator fault; cli/build.rs refuses a cross build for
  this). With MACOS_X64_ON_ARM=1 (the repository variable; or true / on /
  yes) and RUNNER_MACOS_ARM set, its `runner` is the arm64 pool: the Mac
  mini builds it far faster than the MacBook's 1-1.5 h a leg. The cli job's
  smoke tests run the x86_64 binary there under Rosetta too, which the pool's
  image has (ops provision-rosetta-guest.sh). `os` stays macos-15-intel (the
  caches and artifacts are named after it; the overflow builds there,
  natively), and `gui_runner` is unchanged: the jobs that launch the app
  (package, e2e_fork) stay where they were. The x86_64 legs then take the
  arm64 pool's slots like its own.
- `runs_on`: {job: runs-on} for this run, each job's pool above or, for the
  overflow, the image (`os`). The jobs read it first (`matrix.runs_on.cli ||
  matrix.runner || matrix.os`).

The overflow. A pool has few slots (the mac-x64 pool one), and a target's legs
on it queue behind each other: denort and denort bin start together after
setup, cli after denort bin, so a release ran denort, denort bin and cli one
after another on the Intel Mac (about 4 hours). Each pool starts with its free
slots: DENEXT_POOL_HEALTH's `healthy_slots` (else `slots`) minus the `busy` of
its label and of the labels that share its capacity (SHARED_LABELS), when the
value is fresh and lists the label; none when it marks the pool not ok or
paused; else the slot count variable (RUNNER_*_SLOTS, default 1). Then, the
jobs in dependency order:

- a job whose `needs` (same target) held a slot of its pool takes that slot
  over: it starts the moment that job ends and frees it, so it waits for
  nothing;
- else it takes a free slot, if one is left;
- else the image: on the pool it would only wait for a slot.

The jobs that start together (after setup) take the free slots first those
with the longest chain of later jobs on the same pool (denort bin, whose cli
follows it on the mac-x64 pool, before denort, whose package launches on the
image), so the slot passes down a whole chain. On the 1-slot mac-x64 pool:
denort bin and then cli on the pool, denort on macos-15-intel, in parallel.
A job the run skips (SKIP_JOBS) holds no slot. Our runners first: the image
only takes a job that would otherwise wait.

Env: TARGETS ("all" or a comma-separated list of triples),
     RUNNER_MACOS_ARM, RUNNER_MACOS_X64, RUNNER_MACOS_ARM_GUI,
     RUNNER_MACOS_X64_GUI, RUNNER_LINUX_X64_2204, MACOS_X64_ON_ARM (each
     optional),
     RUNNER_MACOS_ARM_SLOTS, RUNNER_MACOS_X64_SLOTS,
     RUNNER_LINUX_X64_2204_SLOTS (each optional, default 1),
     POOL_HEALTH (the organization variable DENEXT_POOL_HEALTH, optional),
     SKIP_JOBS (space-separated job keys this run skips, optional).
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pick_runner import fresh_pools, parse_runner  # noqa: E402

ALL_TARGETS = [
    {"target": "aarch64-apple-darwin", "os": "macos-15"},
    {"target": "x86_64-apple-darwin", "os": "macos-15-intel"},
    {"target": "x86_64-unknown-linux-gnu", "os": "ubuntu-22.04"},
    {"target": "aarch64-unknown-linux-gnu", "os": "ubuntu-22.04-arm"},
    {"target": "x86_64-pc-windows-msvc", "os": "windows-2025"},
    {"target": "aarch64-pc-windows-msvc", "os": "windows-11-arm"},
]


# The matrix jobs that run on a target's pool: (job key, the matrix field
# naming the pool, the jobs it needs in the same target). In dependency order
# (denext_runtime.yml's `needs`). rpm-smoke (Linux only, `runner`, always the
# image) is not one.
JOBS = (
    ("laufey", "laufey_runner", ()),
    ("denort", "runner", ()),
    ("denort_bin", "runner", ()),
    ("cli", "runner", ("denort_bin",)),
    ("package", "gui_runner", ("laufey", "denort")),
    ("e2e_fork", "gui_runner", ("package", "cli")),
)

# The pool variables and their slot count variables.
SLOT_VARS = (
    ("RUNNER_MACOS_ARM", "RUNNER_MACOS_ARM_SLOTS"),
    ("RUNNER_MACOS_X64", "RUNNER_MACOS_X64_SLOTS"),
    ("RUNNER_LINUX_X64_2204", "RUNNER_LINUX_X64_2204_SLOTS"),
)

# Labels whose jobs take the same slots (biscuits' Linux pools share one
# capacity; denext_tests.yml's pick_shared_labels for the same pool).
SHARED_LABELS = {
    "denext-linux-x64-2204": ("denext-linux-x64", "denext-linux-x64-docker"),
}

IMAGES = {t["os"] for t in ALL_TARGETS}


def _runner(value, image):
    value = (value or "").strip()
    if value.startswith("["):
        return json.loads(value)
    return value or image


def _label(runner):
    """The label that identifies a pool, or None for a GitHub image."""
    if isinstance(runner, str):
        if runner in IMAGES or runner.split("-")[0] in ("ubuntu", "macos", "windows"):
            return None
        return runner
    return parse_runner(json.dumps(runner))[1]


def _int(value):
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def free_slots(label, env, pools):
    """The slots of the pool `label` this run may take (see the docstring)."""
    entry = pools.get(label) if pools is not None else None
    if isinstance(entry, dict):
        if entry.get("ok") is False or entry.get("paused") is True:
            return 0
        cap = _int(entry.get("healthy_slots"))
        if cap is None:
            cap = _int(entry.get("slots"))
        if cap is not None:
            busy = 0
            for l in (label, *SHARED_LABELS.get(label, ())):
                e = pools.get(l)
                if isinstance(e, dict):
                    busy += max(0, _int(e.get("busy")) or 0)
            return max(0, cap - busy)
    for var, slots_var in SLOT_VARS:
        if _label(_runner(env.get(var, ""), "")) == label:
            try:
                return max(0, int(env.get(slots_var) or "1"))
            except ValueError:
                return 1
    return 1


def allocate(targets, env):
    """Sets each target's `runs_on` (see the docstring)."""
    skip = set((env.get("SKIP_JOBS") or "").split())
    pools = fresh_pools(env.get("POOL_HEALTH"))
    jobs = [j for j in JOBS if j[0] not in skip]
    needs = {k: [n for n in ns if n not in skip] for k, _, ns in jobs}
    field = {k: f for k, f, _ in jobs}
    depth = {}
    for k, _, _ in jobs:
        depth[k] = 1 + max((depth[n] for n in needs[k]), default=-1)
    legs = []
    for ti, t in enumerate(targets):
        t["runs_on"] = {}
        label = {k: _label(t[field[k]]) for k, _, _ in jobs}

        def chain(k):
            return 1 + max(
                (chain(s) for s, _, _ in jobs if k in needs[s] and label[s] == label[k]),
                default=0,
            )

        for ji, (k, _, _) in enumerate(jobs):
            legs.append((depth[k], -chain(k), ti, ji, t, k, label))
    legs.sort(key=lambda leg: leg[:4])
    free = {}
    held = {}  # (target index, job): the leg holds a slot not yet passed on
    for _, _, ti, _, t, k, label in legs:
        pool = label[k]
        on_pool = False
        if pool is not None:
            heir = next((n for n in needs[k] if held.get((ti, n)) == pool), None)
            if heir is not None:
                del held[(ti, heir)]
                on_pool = True
            else:
                if pool not in free:
                    free[pool] = free_slots(pool, env, pools)
                if free[pool] > 0:
                    free[pool] -= 1
                    on_pool = True
        if on_pool:
            held[(ti, k)] = pool
        t["runs_on"][k] = t[field[k]] if on_pool else t["os"]
    for t in targets:
        t["runs_on"] = {k: t["runs_on"][k] for k, _, _ in jobs}
    return targets


def x64_on_arm(env):
    """Whether x86_64-apple-darwin builds on the arm64 pool under Rosetta 2
    (`rosetta` in the module docstring): MACOS_X64_ON_ARM on and an arm64
    pool."""
    if not (env.get("RUNNER_MACOS_ARM") or "").strip():
        return False
    flag = (env.get("MACOS_X64_ON_ARM") or "").strip().lower()
    return flag in ("1", "true", "on", "yes")


def apply_rosetta(targets, env):
    """Sets each target's `rosetta`; such a target's `runner` becomes the
    arm64 pool. Before allocate(), so its legs take that pool's slots."""
    on = x64_on_arm(env)
    for t in targets:
        t["rosetta"] = on and t["target"] == "x86_64-apple-darwin"
        if t["rosetta"]:
            t["runner"] = _runner(env.get("RUNNER_MACOS_ARM", ""), "macos-15")
    return targets


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
    return allocate(apply_rosetta(targets, env), env)


if __name__ == "__main__":
    json.dump(plan(os.environ), sys.stdout)
    sys.stdout.write("\n")
