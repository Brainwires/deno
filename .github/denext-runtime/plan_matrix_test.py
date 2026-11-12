#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests of plan_matrix.py and of the denext_runtime.yml jobs that use it.
Run: python3 -I .github/denext-runtime/plan_matrix_test.py"""
import json
import os
import re
import subprocess
import sys
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import plan_matrix  # noqa: E402

WORKFLOW = os.path.join(HERE, "..", "workflows", "denext_runtime.yml")

# The repository variables as they were for the denext.14 release.
POOLS = {
    "RUNNER_MACOS_ARM": '["self-hosted","denext-macos-arm64"]',
    "RUNNER_MACOS_X64": '["self-hosted","denext-macos-x64"]',
}
# POOLS, ARM and X64 must name the same pools.


ARM = ["self-hosted", "denext-macos-arm64"]
X64 = ["self-hosted", "denext-macos-x64"]
L2204 = ["self-hosted", "denext-linux-x64-2204"]


def by_target(matrix):
    return {t["target"]: t for t in matrix}


def health(pools, age=60):
    """A DENEXT_POOL_HEALTH value checked `age` seconds ago."""
    return json.dumps({"v": 1, "checked_at": int(time.time()) - age, "pools": pools})


def entry(slots=1, busy=0, ok=True, paused=False, healthy_slots=None):
    e = {"ok": ok, "reason": "", "since": 0, "slots": slots, "busy": busy, "paused": paused}
    if healthy_slots is not None:
        e["healthy_slots"] = healthy_slots
    return e


def on_pool(target):
    """The jobs of `target` that run on a pool (not its image)."""
    return sorted(k for k, r in target["runs_on"].items() if r != target["os"])


class Plan(unittest.TestCase):
    def test_no_variables_is_every_image(self):
        m = by_target(plan_matrix.plan({"TARGETS": "all"}))
        self.assertEqual(len(m), 6)
        for t in m.values():
            self.assertEqual(t["runner"], t["os"])
            self.assertEqual(t["gui_runner"], t["os"])

    def test_x64_builds_on_the_pool_but_launches_on_the_image(self):
        m = by_target(plan_matrix.plan({"TARGETS": "all", **POOLS}))
        x64 = m["x86_64-apple-darwin"]
        self.assertEqual(x64["runner"], ["self-hosted", "denext-macos-x64"])
        # The MacBook pool has no GUI session: the app it launched aborted.
        self.assertEqual(x64["gui_runner"], "macos-15-intel")

    def test_arm64_launches_on_its_pool(self):
        m = by_target(plan_matrix.plan({"TARGETS": "all", **POOLS}))
        arm = m["aarch64-apple-darwin"]
        self.assertEqual(arm["runner"], ["self-hosted", "denext-macos-arm64"])
        self.assertEqual(arm["gui_runner"], ["self-hosted", "denext-macos-arm64"])

    def test_gui_variables_override(self):
        env = {
            "TARGETS": "all",
            **POOLS,
            "RUNNER_MACOS_X64_GUI": "some-gui-label",
            "RUNNER_MACOS_ARM_GUI": "macos-15",
        }
        m = by_target(plan_matrix.plan(env))
        self.assertEqual(m["x86_64-apple-darwin"]["gui_runner"], "some-gui-label")
        self.assertEqual(m["aarch64-apple-darwin"]["gui_runner"], "macos-15")

    def test_non_mac_targets_ignore_the_variables(self):
        m = by_target(plan_matrix.plan({"TARGETS": "all", **POOLS}))
        for name in ("x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"):
            self.assertEqual(m[name]["runner"], m[name]["os"])
            self.assertEqual(m[name]["gui_runner"], m[name]["os"])

    def test_linux_x64_builds_laufey_on_the_2204_pool(self):
        env = {
            "TARGETS": "all",
            **POOLS,
            "RUNNER_LINUX_X64_2204": '["self-hosted","denext-linux-x64-2204"]',
        }
        m = by_target(plan_matrix.plan(env))
        self.assertEqual(
            m["x86_64-unknown-linux-gnu"]["laufey_runner"],
            ["self-hosted", "denext-linux-x64-2204"],
        )
        # Its release builds stay on the image (swap, the glibc floor).
        self.assertEqual(m["x86_64-unknown-linux-gnu"]["runner"], "ubuntu-22.04")
        for name, t in m.items():
            if name != "x86_64-unknown-linux-gnu":
                self.assertEqual(t["laufey_runner"], t["os"], name)

    def test_laufey_without_the_variable_is_the_image(self):
        m = by_target(plan_matrix.plan({"TARGETS": "all"}))
        for t in m.values():
            self.assertEqual(t["laufey_runner"], t["os"])

    def test_the_laufey_job_uses_the_laufey_runner(self):
        with open(WORKFLOW) as f:
            text = f.read()
        job = text.split("\n  laufey:\n", 1)[1].split("\n  denort:\n", 1)[0]
        self.assertIn("runs-on: ${{ matrix.runs_on.laufey || matrix.laufey_runner || matrix.os }}", job)
        self.assertIn("RUNNER_LINUX_X64_2204: ${{ vars.RUNNER_LINUX_X64_2204 }}", text)

    def test_target_selection(self):
        m = plan_matrix.plan({"TARGETS": " x86_64-apple-darwin ,", **POOLS})
        self.assertEqual([t["target"] for t in m], ["x86_64-apple-darwin"])
        with self.assertRaises(SystemExit):
            plan_matrix.plan({"TARGETS": "x86_64-apple-ios"})


class Rosetta(unittest.TestCase):
    """x86_64-apple-darwin built on the arm64 pool under Rosetta 2 (MACOS_X64_ON_ARM)."""

    def plan(self, **env):
        return by_target(plan_matrix.plan({"TARGETS": "all", **POOLS, **env}))

    def test_off_by_default(self):
        for t in self.plan().values():
            self.assertFalse(t["rosetta"], t["target"])
        self.assertEqual(self.plan()["x86_64-apple-darwin"]["runner"], X64)

    def test_on_moves_the_build_jobs_to_the_arm_pool(self):
        m = self.plan(MACOS_X64_ON_ARM="1", RUNNER_MACOS_ARM_SLOTS="4")
        x64 = m["x86_64-apple-darwin"]
        self.assertTrue(x64["rosetta"])
        self.assertEqual(x64["runner"], ARM)
        for job in ("denort", "denort_bin", "cli"):
            self.assertEqual(x64["runs_on"][job], ARM, job)
        # The jobs that launch the app stay native; the image names stay.
        self.assertEqual(x64["os"], "macos-15-intel")
        self.assertEqual(x64["gui_runner"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["package"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["e2e_fork"], "macos-15-intel")
        for name, t in m.items():
            if name != "x86_64-apple-darwin":
                self.assertFalse(t["rosetta"], name)

    def test_the_x64_legs_share_the_arm_slots(self):
        # Two slots: arm64's chains come first (lower target index at equal
        # depth and chain); the x64 legs that would wait overflow to their
        # image (macos-15-intel), which builds them natively.
        m = self.plan(MACOS_X64_ON_ARM="1", RUNNER_MACOS_ARM_SLOTS="2")
        x64 = m["x86_64-apple-darwin"]
        arm = m["aarch64-apple-darwin"]
        self.assertEqual(arm["runs_on"]["denort"], ARM)
        self.assertEqual(arm["runs_on"]["denort_bin"], ARM)
        self.assertEqual(x64["runs_on"]["denort"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["denort_bin"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["cli"], "macos-15-intel")

    def test_spellings(self):
        for on in ("1", "true", "ON", " yes "):
            self.assertTrue(self.plan(MACOS_X64_ON_ARM=on)["x86_64-apple-darwin"]["rosetta"], on)
        for off in ("", "0", "false", "off", "maybe"):
            self.assertFalse(self.plan(MACOS_X64_ON_ARM=off)["x86_64-apple-darwin"]["rosetta"], off)

    def test_no_arm_pool_means_the_x64_pool(self):
        env = {"TARGETS": "all", "RUNNER_MACOS_X64": POOLS["RUNNER_MACOS_X64"], "MACOS_X64_ON_ARM": "1"}
        x64 = by_target(plan_matrix.plan(env))["x86_64-apple-darwin"]
        self.assertFalse(x64["rosetta"])
        self.assertEqual(x64["runner"], X64)

    def test_a_bare_label(self):
        env = {"TARGETS": "x86_64-apple-darwin", "RUNNER_MACOS_ARM": "mini", "MACOS_X64_ON_ARM": "1"}
        (x64,) = plan_matrix.plan(env)
        self.assertEqual(x64["runner"], "mini")
        self.assertTrue(x64["rosetta"])


class Overflow(unittest.TestCase):
    """The runs_on of each job: the pool while it has a slot, else the image."""

    def plan(self, **env):
        return by_target(plan_matrix.plan({"TARGETS": "all", **POOLS, **env}))

    def test_no_variables_is_every_image(self):
        for t in plan_matrix.plan({"TARGETS": "all"}):
            self.assertEqual(set(t["runs_on"].values()), {t["os"]})
            self.assertEqual(
                list(t["runs_on"]), [k for k, _, _ in plan_matrix.JOBS]
            )

    def test_one_slot_mac_x64_chains_denort_bin_and_cli(self):
        # The release that took 4 hours: denort, denort bin and cli one after
        # another on the MacBook's one slot. denort bin keeps the slot and
        # passes it to cli (which needs it); denort, which would only wait,
        # builds on the image at the same time.
        x64 = self.plan()["x86_64-apple-darwin"]
        self.assertEqual(x64["runs_on"]["denort_bin"], X64)
        self.assertEqual(x64["runs_on"]["cli"], X64)
        self.assertEqual(x64["runs_on"]["denort"], "macos-15-intel")
        # No GUI session on that pool: package and e2e stay on the image.
        self.assertEqual(x64["runs_on"]["package"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["e2e_fork"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["laufey"], "macos-15-intel")

    def test_two_slot_mac_arm_keeps_every_leg(self):
        arm = self.plan(RUNNER_MACOS_ARM_SLOTS="2")["aarch64-apple-darwin"]
        self.assertEqual(on_pool(arm), ["cli", "denort", "denort_bin", "e2e_fork", "package"])
        self.assertEqual(arm["runs_on"]["laufey"], "macos-15")

    def test_one_slot_mac_arm_passes_its_slot_down_one_chain(self):
        arm = self.plan(RUNNER_MACOS_ARM_SLOTS="1")["aarch64-apple-darwin"]
        # denort -> package -> e2e_fork keeps the slot; denort bin and cli,
        # which would wait for it, take macos-15.
        self.assertEqual(on_pool(arm), ["denort", "e2e_fork", "package"])
        self.assertEqual(arm["runs_on"]["denort_bin"], "macos-15")
        self.assertEqual(arm["runs_on"]["cli"], "macos-15")

    def test_no_two_legs_wait_for_one_slot(self):
        # Whatever the slots, the jobs that start together (after setup) on a
        # pool never outnumber its slots.
        for slots in ("0", "1", "2", "3"):
            m = self.plan(RUNNER_MACOS_ARM_SLOTS=slots, RUNNER_MACOS_X64_SLOTS=slots)
            for name in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
                first = [k for k in ("denort", "denort_bin") if k in on_pool(m[name])]
                self.assertLessEqual(len(first), int(slots), (name, slots))

    def test_zero_slots_is_every_image(self):
        x64 = self.plan(RUNNER_MACOS_X64_SLOTS="0")["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), [])

    def test_slot_variable_garbage_is_one(self):
        x64 = self.plan(RUNNER_MACOS_X64_SLOTS="x")["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort_bin"])

    def test_laufey_takes_the_linux_pool(self):
        lx = self.plan(RUNNER_LINUX_X64_2204=json.dumps(L2204))["x86_64-unknown-linux-gnu"]
        self.assertEqual(lx["runs_on"]["laufey"], L2204)
        self.assertEqual(on_pool(lx), ["laufey"])

    def test_health_busy_slots_overflow(self):
        h = health({"denext-macos-x64": entry(slots=1, busy=1, healthy_slots=1)})
        x64 = self.plan(POOL_HEALTH=h)["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), [])

    def test_health_slots_override_the_variable(self):
        # Two free healthy slots: denort and denort bin both start on the pool.
        h = health({"denext-macos-x64": entry(slots=3, busy=1, healthy_slots=3)})
        x64 = self.plan(POOL_HEALTH=h, RUNNER_MACOS_X64_SLOTS="1")["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort", "denort_bin"])

    def test_health_healthy_slots_before_slots(self):
        # One of two hosts on the label is down: one healthy slot.
        h = health({"denext-macos-x64": entry(slots=2, busy=0, healthy_slots=1)})
        x64 = self.plan(POOL_HEALTH=h)["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort_bin"])

    def test_health_without_healthy_slots_uses_slots(self):
        h = health({"denext-macos-x64": entry(slots=2, busy=0)})
        x64 = self.plan(POOL_HEALTH=h)["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort", "denort_bin"])

    def test_unhealthy_or_paused_pool_sends_everything_to_the_image(self):
        for e in (entry(slots=2, ok=False), entry(slots=2, paused=True)):
            h = health({"denext-macos-arm64": e})
            arm = self.plan(POOL_HEALTH=h, RUNNER_MACOS_ARM_SLOTS="2")["aarch64-apple-darwin"]
            self.assertEqual(on_pool(arm), [], e)

    def test_stale_or_garbage_health_uses_the_variable(self):
        stale = health({"denext-macos-x64": entry(ok=False)}, age=3600)
        for h in (stale, "", "not json", "[]", json.dumps({"checked_at": True})):
            x64 = self.plan(POOL_HEALTH=h)["x86_64-apple-darwin"]
            self.assertEqual(on_pool(x64), ["cli", "denort_bin"], h)

    def test_health_without_the_label_uses_the_variable(self):
        h = health({"denext-linux-x64": entry(slots=3, busy=3)})
        x64 = self.plan(POOL_HEALTH=h)["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort_bin"])

    def test_shared_labels_take_the_same_slots(self):
        h = health({
            "denext-linux-x64-2204": entry(slots=3, busy=0, healthy_slots=3),
            "denext-linux-x64": entry(slots=3, busy=2, healthy_slots=3),
            "denext-linux-x64-docker": entry(slots=1, busy=1, healthy_slots=1),
        })
        lx = self.plan(POOL_HEALTH=h, RUNNER_LINUX_X64_2204=json.dumps(L2204))
        self.assertEqual(on_pool(lx["x86_64-unknown-linux-gnu"]), [])

    def test_cli_only_skips_the_runtime_jobs(self):
        m = self.plan(SKIP_JOBS="laufey denort package e2e_fork")
        x64 = m["x86_64-apple-darwin"]
        self.assertEqual(list(x64["runs_on"]), ["denort_bin", "cli"])
        self.assertEqual(on_pool(x64), ["cli", "denort_bin"])

    def test_a_reused_denort_holds_no_slot(self):
        arm = self.plan(SKIP_JOBS="denort", RUNNER_MACOS_ARM_SLOTS="1")["aarch64-apple-darwin"]
        # denort bin -> cli -> e2e_fork; package starts after laufey (on the
        # image) while cli holds the slot.
        self.assertEqual(on_pool(arm), ["cli", "denort_bin", "e2e_fork"])
        self.assertEqual(arm["runs_on"]["package"], "macos-15")

    def test_only_the_selected_targets_count(self):
        m = plan_matrix.plan({"TARGETS": "x86_64-apple-darwin", **POOLS})
        self.assertEqual(on_pool(m[0]), ["cli", "denort_bin"])

    def test_a_gui_pool_of_its_own(self):
        x64 = self.plan(RUNNER_MACOS_X64_GUI="some-gui-label")["x86_64-apple-darwin"]
        self.assertEqual(x64["runs_on"]["package"], "some-gui-label")
        self.assertEqual(x64["runs_on"]["e2e_fork"], "some-gui-label")

    def test_an_image_named_by_a_variable_is_no_pool(self):
        arm = self.plan(RUNNER_MACOS_ARM_GUI="macos-15", RUNNER_MACOS_ARM_SLOTS="2")
        self.assertEqual(arm["aarch64-apple-darwin"]["runs_on"]["package"], "macos-15")

    def test_runs_isolated(self):
        # The plan step runs it with -I, which leaves the script's directory
        # off sys.path: the pick_runner import must still resolve.
        out = subprocess.run(
            [sys.executable, "-I", os.path.join(HERE, "plan_matrix.py")],
            env={"TARGETS": "x86_64-apple-darwin", **POOLS},
            capture_output=True, text=True, check=True,
        ).stdout
        self.assertEqual(json.loads(out)[0]["runs_on"]["cli"], X64)


def job_block(text, job):
    """The text of one top-level job of the workflow."""
    m = re.search(rf"^  {re.escape(job)}:\n(.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)", text, re.M | re.S)
    if not m:
        raise AssertionError(f"no job {job} in denext_runtime.yml")
    return m.group(1)


def runs_on(block):
    m = re.search(r"^    runs-on: (.*)$", block, re.M)
    if not m:
        raise AssertionError("job has no runs-on")
    return m.group(1)


class Workflow(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(WORKFLOW, encoding="utf-8") as f:
            cls.text = f.read()

    def test_the_plan_step_uses_plan_matrix(self):
        setup = job_block(self.text, "setup")
        self.assertIn(".github/denext-runtime/plan_matrix.py", setup)
        self.assertIn("python3 -I .github/denext-runtime/plan_matrix.py", setup)
        for var in ("RUNNER_MACOS_ARM_GUI", "RUNNER_MACOS_X64_GUI", "MACOS_X64_ON_ARM"):
            self.assertIn(f"{var}: ${{{{ vars.{var} }}}}", setup)

    def test_jobs_that_launch_the_app_use_the_gui_runner(self):
        for job in ("package", "e2e_fork"):
            r = runs_on(job_block(self.text, job))
            self.assertIn("matrix.gui_runner", r, job)
            self.assertNotIn("matrix.runner ", r, job)

    def test_rosetta_builds_use_the_x86_64_toolchain(self):
        # Not --target: the snapshot build scripts must run an x86_64 V8.
        for job in ("denort", "denort_bin", "cli"):
            block = job_block(self.text, job)
            self.assertIn('if [ "${{ matrix.rosetta }}" = true ]; then\n            tc="$tc-x86_64-apple-darwin"', block, job)
            self.assertIn('bash .github/denext-runtime/rosetta_tools.sh "$RUNNER_TEMP/rosetta-tools" "$GITHUB_ENV"', block, job)
            self.assertIn('echo "$RUNNER_TEMP/rosetta-tools" >>"$GITHUB_PATH"', block, job)
            self.assertNotIn("CARGO_BUILD_TARGET", block, job)
            self.assertNotIn("DENO_SKIP_CROSS_BUILD_CHECK", block, job)
            self.assertIn("lipo -archs", block, job)

    def test_build_jobs_keep_the_build_runner(self):
        for job in ("denort", "denort_bin", "cli"):
            r = runs_on(job_block(self.text, job))
            self.assertIn("matrix.runner", r, job)
            self.assertNotIn("gui_runner", r, job)

    def test_every_pooled_job_reads_its_pick_first(self):
        for job, field, _ in plan_matrix.JOBS:
            r = runs_on(job_block(self.text, job))
            self.assertIn(f"matrix.runs_on.{job} || matrix.{field} || matrix.os", r, job)

    def test_the_plan_step_passes_the_overflow_inputs(self):
        setup = job_block(self.text, "setup")
        self.assertIn(".github/denext-runtime/pick_runner.py", setup)
        self.assertIn("POOL_HEALTH: ${{ vars.DENEXT_POOL_HEALTH }}", setup)
        self.assertIn("SKIP_JOBS:", setup)
        for _, slots_var in plan_matrix.SLOT_VARS:
            self.assertIn(f"{slots_var}: ${{{{ vars.{slots_var} ||", setup)

    def test_slot_defaults_match_the_tests_workflow(self):
        # The same pools and defaults as denext_tests.yml's pick_slots.
        with open(os.path.join(HERE, "..", "workflows", "denext_tests.yml"), encoding="utf-8") as f:
            tests = f.read()
        setup = job_block(self.text, "setup")
        for _, slots_var in plan_matrix.SLOT_VARS:
            want = re.search(rf"vars\.{slots_var} \|\| '(\d+)'", tests).group(1)
            got = re.search(rf"vars\.{slots_var} \|\| '(\d+)'", setup).group(1)
            self.assertEqual(got, want, slots_var)
        shared = re.search(r"pick_shared_labels: (.*)$", tests, re.M).group(1).split()
        self.assertEqual(sorted(plan_matrix.SHARED_LABELS["denext-linux-x64-2204"]), sorted(shared))


if __name__ == "__main__":
    unittest.main()
