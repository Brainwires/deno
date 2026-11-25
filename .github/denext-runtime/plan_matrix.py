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
  the image. The Linux and Windows build jobs always take the image: a
  release build (fat LTO, codegen-units=1) peaks above GitHub's 16 GB (the
  aarch64 Linux legs add a 16 GB swapfile to finish), while our Windows hosts
  have 8 GB (win-x64's VM, win-x64-brain's i3, each win-arm64 VM) and our
  Linux job containers 3.5 GB + swap to 7 GB (biscuits) or 6 GB + 2 GB (the
  mini); and the Linux jobs `fallocate` + `swapon` a swapfile, `mount` the
  glibc 2.27 sysroot and `chroot` into it to run what they built, which the
  containers refuse (no SYS_ADMIN, no SYS_CHROOT; ops/runners README).
- `gui_runner`: where the jobs that launch the packaged app run (e2e_fork,
  and package unless `package_runner` says otherwise: their smoke and e2e
  open windows). Launching an app needs a GUI login session for the job's
  user.
  macOS: the mac-arm pool has one: its Tart guests log `runner` in at the
  console. The mac-x64 pool has none: the MacBook runs its jobs as `runner`
  over ssh + sudo inside `sandbox-exec`. There AppKit aborts the app
  (`xpc_connection_set_target_uid` API misuse, SIGILL; or SIGSEGV in
  SLSNewConnection), and the sandbox refuses to exec the setuid /bin/ps the
  e2e runner lists processes with (EPERM). That was the denext.14 release
  failure. The arm64 GUI runner follows RUNNER_MACOS_ARM unless
  RUNNER_MACOS_ARM_GUI overrides it. Never the mac-x64 pool
  (`denext-macos-x64`), even when RUNNER_MACOS_X64_GUI names it: since
  2026-11-18 its jobs are denied Accessibility, Screen Recording, Input
  Monitoring, Camera, Microphone and Automation without a prompt (the
  MacBook's console session is the user's; ops/runners README "Permissions
  on mac-x64"), and the e2e posts key events through the Accessibility
  grant. x86_64-apple-darwin's launch jobs run on the arm64 GUI pool, the
  x86_64 app under Rosetta 2, whenever RUNNER_MACOS_ARM_GUI or
  RUNNER_MACOS_ARM names a pool, unless MACOS_X64_GUI_ON_ARM is 0 / false /
  off / no. The Tart guests pre-grant every privacy permission (ops/runners
  tart/provision-tcc-guest.sh) and already run the x86_64 cli's smoke tests
  under Rosetta (`rosetta` below). That is the default since denext.31 (it
  was opt-in): otherwise those two legs always ran on macos-15-intel while
  no pool of ours could ever take them. A pool RUNNER_MACOS_X64_GUI names
  (other than the mac-x64 pool) wins over that default;
  MACOS_X64_GUI_ON_ARM=1 (or true / on / yes) wins over
  RUNNER_MACOS_X64_GUI. With the default turned off and no such pool: the
  image.
  Linux: the launch jobs run under Xvfb (and a headless sway for the Wayland
  variant) with their own session bus and gnome-keyring. The Ubuntu 22.04
  pool images carry all of it (ops/runners linux/Dockerfile.2204 and
  linux-arm64/Dockerfile: xvfb, sway, dbus-x11, gnome-keyring, gcr, xfwm4,
  xdotool, wmctrl and the WebKitGTK / CEF runtime libraries), so
  apt_install.sh fetches nothing; laufey's own CEF / webview window tests
  and denext's 22.04 desktop-window entries run there already.
  x86_64-unknown-linux-gnu launches on RUNNER_LINUX_X64_GUI, else the Ubuntu
  22.04 pool (RUNNER_LINUX_X64_2204); aarch64-unknown-linux-gnu on
  RUNNER_LINUX_ARM64_GUI, else the Linux arm64 pool (RUNNER_LINUX_ARM64).
  Windows: both Windows pools run each job in the job account's console
  session (ops/runners windows-x64/README.md "Desktop session",
  windows/README.md). x86_64-pc-windows-msvc launches on
  RUNNER_WINDOWS_X64_GUI, else RUNNER_WINDOWS_X64; aarch64-pc-windows-msvc
  on RUNNER_WINDOWS_ARM_GUI, else RUNNER_WINDOWS_ARM.
  A *_GUI variable that names a GitHub image (`ubuntu-22.04`,
  `windows-2025`, ...) sends that target's launch jobs to the image: the
  switch to turn a pool off for them.
- `package_runner`: where the package job runs: `gui_runner`, except on
  Windows, where it is the image. Its e2e installs the stock CLI's `.msi`
  per machine (the deeplink area: `msiexec /i /qn` into Program Files),
  which needs an administrator. GitHub's Windows images run jobs as one
  with UAC off; our Windows pools run them as the non-admin `runner`
  account with UAC on (ops/runners windows-x64/README.md: "No job category
  here needs admin"), so that check would fail there. e2e_fork (the
  sidecarint and kernelsandbox areas, no installer) takes the Windows pools.
- `laufey_runner`: where the laufey job builds the backend hosts.
  x86_64-unknown-linux-gnu: the Ubuntu 22.04 Linux pool
  (RUNNER_LINUX_X64_2204); aarch64-unknown-linux-gnu: the Linux arm64 pool
  (RUNNER_LINUX_ARM64). Both images carry every package that job installs
  (the same Ubuntu 22.04 package list), so it downloads none. The Windows
  legs: RUNNER_WINDOWS_X64 / RUNNER_WINDOWS_ARM. The job installs make 4.4.1
  and ninja 1.13.2 with Chocolatey where there is one (GitHub's images); our
  Windows pools have none, so there it unpacks the same two Chocolatey
  packages itself (pinned SHA-256: the bytes `choco install` fetches). VS
  Build Tools with CMake, the Windows SDK, Git Bash and PowerShell 7 are on
  both pools. The macOS legs stay on the image: x86_64-apple-darwin's
  because the mac-x64 pool has no Homebrew its job user can install into
  (the leg `brew install`s llvm and ninja) and the mac-arm guests' Homebrew
  is arm64; aarch64-apple-darwin's because the mac-arm pool's two slots are
  the release's critical path (both macOS targets' builds and launch jobs):
  the 5-minute laufey leg starts together with the builds and would take a
  slot from one of them, sending a 45-120 minute build to GitHub instead.
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
  natively), and `gui_runner` does not follow it (its own rule above). The x86_64 legs then take the
  arm64 pool's slots like its own, and when that pool is full they fall back
  to the mac-x64 pool (`runner_fallback`, RUNNER_MACOS_X64) while it has a
  free slot, before the image: the MacBook builds them natively, as the
  image would (rosetta_tools.sh writes no wrappers on an Intel Mac). In run
  43519114893 every x86_64 leg went to macos-15-intel while the mac-x64
  pool sat idle.
- `runner_fallback` (a rosetta target only, when RUNNER_MACOS_X64 names a
  pool): the second pool its build jobs try before the image.
- `runs_on`: {job: runs-on} for this run, each job's pool above or, for the
  overflow, the image (`os`). The jobs read it first (`matrix.runs_on.cli ||
  matrix.runner || matrix.os`). rpm-smoke (Linux only) is not in it and
  always takes the image (`runner`): it runs `docker run` with a bind mount
  of the job's files, which none of our Linux pools can do (no Docker in
  the job containers; linux-x64-docker's daemon is a sidecar that does not
  see the job's filesystem; none on the mini's linux-arm64).

The overflow. A pool has few slots (the mac-x64 pool one), and a target's legs
on it queue behind each other: denort and denort bin start together after
setup, cli after denort bin, so a release ran denort, denort bin and cli one
after another on the Intel Mac (about 4 hours). Each pool starts with its free
slots: POOL_FREE[label] when POOL_FREE parses and lists the label (the shared
picker's `free` output, Brainwires/.github/actions/pick-runners, which the
setup job runs just before this: healthy slots minus the taken ones, the
queued jobs of the repository's other runs included, 0 for a pool marked not
ok or paused or one a re-run found stuck); else DENEXT_POOL_HEALTH's
`healthy_slots` (else `slots`) minus the `busy` of its label and of the labels
that share its capacity (SHARED_LABELS), when the value is fresh and lists the
label; none when it marks the pool not ok or paused; else the slot count
variable (RUNNER_*_SLOTS, default 1). Then, the jobs in dependency order:

- a job whose `needs` (same target) held a slot of its pool (or of its
  `runner_fallback` pool) takes that slot over: it starts the moment that job
  ends and frees it, so it waits for nothing;
- else it takes a free slot of its pool, if one is left;
- else a free slot of its `runner_fallback` pool, if it has one;
- else the image: on a pool it would only wait for a slot;
- except a macOS job once MACOS_CAP (default 3) macOS jobs of this run
  OVERFLOWED to GitHub's images (GitHub runs about 5 macOS jobs at once for
  the whole org, as the shared picker's macos-cap): it waits on its first
  pool that is merely busy. Only overflow counts: a leg with a pool that had
  no free slot. A macOS job with no pool at all (the arm64 legs while
  RUNNER_MACOS_ARM names an image, x86_64-apple-darwin's package and
  e2e_fork with MACOS_X64_GUI_ON_ARM off, which need a GUI session the
  mac-x64 pool lacks, the macOS laufey legs) takes the image and does not
  count. Linux and Windows jobs never count and never wait: past their
  pools' free slots they take the image. It never waits on a pool
  the shared picker reports blocked (POOL_BLOCKED: unhealthy, paused,
  unlisted, or stalled in the previous attempt): such a leg takes the image.
  (denext.24 run 43756144777: three pool-less arm64 legs spent the cap, and
  both x86_64 denort legs were then forced onto the busy, stalled mac-x64.)

The jobs that start together (after setup) take the free slots first those
with the longest chain of later jobs on the same pool (denort bin, whose cli
follows it on the mac-x64 pool, before denort, whose package launches on the
image), so the slot passes down a whole chain. On the 1-slot mac-x64 pool:
denort bin and then cli on the pool, denort on macos-15-intel, in parallel.
A job the run skips (SKIP_JOBS) holds no slot. Our runners first: the image
only takes a job that would otherwise wait.

Env: TARGETS ("all" or a comma-separated list of triples),
     RUNNER_MACOS_ARM, RUNNER_MACOS_X64, RUNNER_MACOS_ARM_GUI,
     RUNNER_MACOS_X64_GUI, RUNNER_LINUX_X64_2204, RUNNER_LINUX_ARM64,
     RUNNER_WINDOWS_X64, RUNNER_WINDOWS_ARM, RUNNER_LINUX_X64_GUI,
     RUNNER_LINUX_ARM64_GUI, RUNNER_WINDOWS_X64_GUI, RUNNER_WINDOWS_ARM_GUI,
     MACOS_X64_ON_ARM, MACOS_X64_GUI_ON_ARM (each optional),
     RUNNER_MACOS_ARM_SLOTS, RUNNER_MACOS_X64_SLOTS,
     RUNNER_LINUX_X64_2204_SLOTS, RUNNER_LINUX_ARM64_SLOTS,
     RUNNER_WINDOWS_X64_SLOTS, RUNNER_WINDOWS_ARM_SLOTS (each optional,
     default 1),
     POOL_FREE (the shared picker's `free` output: {label: free slots},
     optional),
     POOL_HEALTH (the organization variable DENEXT_POOL_HEALTH, optional),
     MACOS_CAP (the most macOS jobs sent to GitHub's images, default 3),
     SKIP_JOBS (space-separated job keys this run skips, optional).
"""
import json
import os
import sys
import time

HEALTH_MAX_AGE = 30 * 60  # seconds: an older DENEXT_POOL_HEALTH is ignored
DEFAULT_MACOS_CAP = 3

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
    ("package", "package_runner", ("laufey", "denort")),
    ("e2e_fork", "gui_runner", ("package", "cli")),
)

# The pool variables and their slot count variables.
SLOT_VARS = (
    ("RUNNER_MACOS_ARM", "RUNNER_MACOS_ARM_SLOTS"),
    ("RUNNER_MACOS_X64", "RUNNER_MACOS_X64_SLOTS"),
    ("RUNNER_LINUX_X64_2204", "RUNNER_LINUX_X64_2204_SLOTS"),
    ("RUNNER_LINUX_ARM64", "RUNNER_LINUX_ARM64_SLOTS"),
    ("RUNNER_WINDOWS_X64", "RUNNER_WINDOWS_X64_SLOTS"),
    ("RUNNER_WINDOWS_ARM", "RUNNER_WINDOWS_ARM_SLOTS"),
)

# Labels whose jobs take the same slots (biscuits' Linux pools share one
# capacity; denext_tests.yml's pick_shared_labels for the same pool).
SHARED_LABELS = {
    "denext-linux-x64-2204": ("denext-linux-x64", "denext-linux-x64-docker"),
}

IMAGES = {t["os"] for t in ALL_TARGETS}


def fresh_pools(value, now=None):
    """DENEXT_POOL_HEALTH's `pools` map ({label: entry}) when `value` (schema
    v1) parses and is fresh, else None (missing, unparsable or stale: the
    plan behaves as without it)."""
    try:
        health = json.loads(value or "")
    except ValueError:
        return None
    if not isinstance(health, dict):
        return None
    at = health.get("checked_at")
    if isinstance(at, bool) or not isinstance(at, (int, float)):
        return None
    age = (time.time() if now is None else now) - at
    if age > HEALTH_MAX_AGE or age < -300:
        return None
    pools = health.get("pools")
    return pools if isinstance(pools, dict) else None


def parse_runner(value):
    """(runs-on JSON, the label that identifies the pool), or (None, None)."""
    v = (value or "").strip()
    if not v:
        return None, None
    if not v.startswith("["):
        return json.dumps(v), v
    try:
        labels = json.loads(v)
    except ValueError:
        return None, None
    if not isinstance(labels, list) or not all(
        isinstance(l, str) and l for l in labels
    ):
        return None, None
    label = next((l for l in labels if l != "self-hosted"), None)
    return (json.dumps(labels, separators=(",", ":")), label) if label else (None, None)


def picked_free(env):
    """POOL_FREE ({label: free slots}, the shared picker's `free` output) when
    it parses as an object, else None."""
    try:
        value = json.loads(env.get("POOL_FREE") or "")
    except ValueError:
        return None
    return value if isinstance(value, dict) else None


def picked_blocked(env):
    """POOL_BLOCKED (the shared picker's `blocked` output: labels that take no
    job) as a set; empty when missing or unparsable."""
    try:
        value = json.loads(env.get("POOL_BLOCKED") or "[]")
    except ValueError:
        return set()
    return {l for l in value if isinstance(l, str)} if isinstance(value, list) else set()


def macos_cap(env):
    try:
        return max(0, int(env.get("MACOS_CAP") or DEFAULT_MACOS_CAP))
    except ValueError:
        return DEFAULT_MACOS_CAP


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


def free_slots(label, env, pools, picked=None):
    """The slots of the pool `label` this run may take (see the docstring):
    `picked` (POOL_FREE) first, then `pools` (DENEXT_POOL_HEALTH), then the
    slot count variable."""
    if picked is not None and _int(picked.get(label)) is not None:
        return max(0, picked[label])
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
    picked = picked_free(env)
    cap = macos_cap(env)
    blocked = picked_blocked(env)
    hosted_macos = 0
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
        # The pools this leg may run on, in order: (label, runs-on).
        candidates = []
        if label[k] is not None:
            candidates.append((label[k], t[field[k]]))
        fallback = t.get("runner_fallback") if field[k] == "runner" else None
        if fallback is not None:
            fl = _label(fallback)
            if fl is not None and fl != label[k]:
                candidates.append((fl, fallback))
        chosen = None
        for pool, runner in candidates:
            heir = next((n for n in needs[k] if held.get((ti, n)) == pool), None)
            if heir is not None:
                del held[(ti, heir)]
                chosen = (pool, runner)
                break
        if chosen is None:
            for pool, runner in candidates:
                if pool not in free:
                    free[pool] = free_slots(pool, env, pools, picked)
                if free[pool] > 0:
                    free[pool] -= 1
                    chosen = (pool, runner)
                    break
        if chosen is None and t["os"].startswith("macos") and candidates:
            # an overflow (the leg had a pool, no slot). Past the cap, wait on a
            # pool that is merely busy; never on a blocked one.
            waitable = [c for c in candidates if c[0] not in blocked]
            if hosted_macos >= cap and waitable:
                chosen = waitable[0]
            else:
                hosted_macos += 1
        if chosen is not None:
            held[(ti, k)] = chosen[0]
        t["runs_on"][k] = chosen[1] if chosen is not None else t["os"]
    for t in targets:
        t["runs_on"] = {k: t["runs_on"][k] for k, _, _ in jobs}
    return targets


# The mac-x64 pool's label: no launch job runs there (see `gui_runner` in the
# module docstring).
MAC_X64_POOL = "denext-macos-x64"


def _flag(env, name):
    return (env.get(name) or "").strip().lower() in ("1", "true", "on", "yes")


def _off(env, name):
    return (env.get(name) or "").strip().lower() in ("0", "false", "off", "no")


def x64_gui(env):
    """Where x86_64-apple-darwin's launch jobs (package, e2e_fork) run (see
    `gui_runner` in the module docstring): the arm64 GUI pool under Rosetta 2
    with MACOS_X64_GUI_ON_ARM on; else RUNNER_MACOS_X64_GUI unless that names
    the mac-x64 pool; else the arm64 GUI pool unless MACOS_X64_GUI_ON_ARM is
    off; else the image ("")."""
    arm = env.get("RUNNER_MACOS_ARM_GUI", "") or env.get("RUNNER_MACOS_ARM", "")
    arm_pool = _label(_runner(arm, "macos-15")) is not None
    if _flag(env, "MACOS_X64_GUI_ON_ARM") and arm.strip():
        return arm
    named = env.get("RUNNER_MACOS_X64_GUI", "")
    if named.strip() and _label(_runner(named, "macos-15-intel")) != MAC_X64_POOL:
        return named
    if arm_pool and not _off(env, "MACOS_X64_GUI_ON_ARM"):
        return arm
    return ""


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
    arm64 pool, and its `runner_fallback` the mac-x64 pool when
    RUNNER_MACOS_X64 names one. Before allocate(), so its legs take those
    pools' slots."""
    on = x64_on_arm(env)
    for t in targets:
        t["rosetta"] = on and t["target"] == "x86_64-apple-darwin"
        if t["rosetta"]:
            native = t["runner"]
            t["runner"] = _runner(env.get("RUNNER_MACOS_ARM", ""), "macos-15")
            if _label(native) is not None:
                t["runner_fallback"] = native
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
        # The mac-x64 pool has no GUI session and denies the privacy
        # permissions (see the module docstring): never the mac-x64 pool here.
        "macos-15-intel": x64_gui(env),
        # Xvfb / headless sway in the Ubuntu 22.04 job containers.
        "ubuntu-22.04": env.get("RUNNER_LINUX_X64_GUI", "")
        or env.get("RUNNER_LINUX_X64_2204", ""),
        "ubuntu-22.04-arm": env.get("RUNNER_LINUX_ARM64_GUI", "")
        or env.get("RUNNER_LINUX_ARM64", ""),
        # The job account's console session on both Windows pools.
        "windows-2025": env.get("RUNNER_WINDOWS_X64_GUI", "")
        or env.get("RUNNER_WINDOWS_X64", ""),
        "windows-11-arm": env.get("RUNNER_WINDOWS_ARM_GUI", "")
        or env.get("RUNNER_WINDOWS_ARM", ""),
    }
    laufey = {
        "x86_64-unknown-linux-gnu": env.get("RUNNER_LINUX_X64_2204", ""),
        "aarch64-unknown-linux-gnu": env.get("RUNNER_LINUX_ARM64", ""),
        "x86_64-pc-windows-msvc": env.get("RUNNER_WINDOWS_X64", ""),
        "aarch64-pc-windows-msvc": env.get("RUNNER_WINDOWS_ARM", ""),
    }
    targets = []
    for t in ALL_TARGETS:
        t = dict(t)
        t["runner"] = _runner(build.get(t["os"], ""), t["os"])
        t["gui_runner"] = _runner(gui.get(t["os"], ""), t["os"])
        # The package job's e2e installs an .msi per machine, which needs an
        # administrator: our Windows pools' job account is none (docstring).
        t["package_runner"] = (
            t["os"] if t["os"].startswith("windows") else t["gui_runner"]
        )
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
