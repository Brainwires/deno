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
    # The slot tests leave the macOS cap out of the way (MacosCap tests it).
    "MACOS_CAP": "99",
    # The tests written for x86_64's native launch path turn off its default,
    # the arm64 pool under Rosetta 2 (GuiOnArm tests that default).
    "MACOS_X64_GUI_ON_ARM": "0",
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
        # depth and chain); with no mac-x64 slot the x64 legs that would wait
        # overflow to their image (macos-15-intel), which builds them
        # natively.
        m = self.plan(
            MACOS_X64_ON_ARM="1", RUNNER_MACOS_ARM_SLOTS="2", RUNNER_MACOS_X64_SLOTS="0"
        )
        x64 = m["x86_64-apple-darwin"]
        arm = m["aarch64-apple-darwin"]
        self.assertEqual(arm["runs_on"]["denort"], ARM)
        self.assertEqual(arm["runs_on"]["denort_bin"], ARM)
        self.assertEqual(x64["runs_on"]["denort"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["denort_bin"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["cli"], "macos-15-intel")

    def test_arm_full_falls_back_to_the_free_x64_pool(self):
        # Run 43519114893: the mac-arm pool full, the mac-x64 pool (one slot)
        # idle, and every x86_64 build leg went to macos-15-intel. The x64
        # pool's slot now takes denort bin and passes it to cli; denort,
        # which would only wait for it, takes the image.
        m = self.plan(MACOS_X64_ON_ARM="1", RUNNER_MACOS_ARM_SLOTS="2")
        x64 = m["x86_64-apple-darwin"]
        self.assertTrue(x64["rosetta"])
        self.assertEqual(x64["runner"], ARM)
        self.assertEqual(x64["runner_fallback"], X64)
        self.assertEqual(x64["runs_on"]["denort_bin"], X64)
        self.assertEqual(x64["runs_on"]["cli"], X64)
        self.assertEqual(x64["runs_on"]["denort"], "macos-15-intel")
        arm = m["aarch64-apple-darwin"]
        self.assertEqual(arm["runs_on"]["denort"], ARM)
        self.assertEqual(arm["runs_on"]["denort_bin"], ARM)

    def test_arm_full_by_health_and_x64_free_by_health(self):
        h = health({
            "denext-macos-arm64": entry(slots=2, busy=2, healthy_slots=2),
            "denext-macos-x64": entry(slots=1, busy=0, healthy_slots=1),
        })
        x64 = self.plan(MACOS_X64_ON_ARM="1", POOL_HEALTH=h)["x86_64-apple-darwin"]
        self.assertEqual(x64["runs_on"]["denort_bin"], X64)
        self.assertEqual(x64["runs_on"]["cli"], X64)
        self.assertEqual(x64["runs_on"]["denort"], "macos-15-intel")

    def test_both_pools_full_is_the_image(self):
        h = health({
            "denext-macos-arm64": entry(slots=2, busy=2, healthy_slots=2),
            "denext-macos-x64": entry(slots=1, busy=1, healthy_slots=1),
        })
        x64 = self.plan(MACOS_X64_ON_ARM="1", POOL_HEALTH=h)["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), [])

    def test_arm_free_keeps_the_arm_pool_first(self):
        m = self.plan(MACOS_X64_ON_ARM="1", RUNNER_MACOS_ARM_SLOTS="4")
        x64 = m["x86_64-apple-darwin"]
        for job in ("denort", "denort_bin", "cli"):
            self.assertEqual(x64["runs_on"][job], ARM, job)

    def test_an_x64_gui_pool_takes_the_launch_jobs(self):
        # RUNNER_MACOS_X64_GUI naming another pool with a GUI session: package
        # and e2e_fork run there while it has a slot.
        gui = '["self-hosted","denext-macos-x64-gui"]'
        m = self.plan(
            MACOS_X64_ON_ARM="1",
            RUNNER_MACOS_ARM_SLOTS="4",
            RUNNER_MACOS_X64_GUI=gui,
            POOL_FREE='{"denext-macos-x64-gui": 1}',
        )
        x64 = m["x86_64-apple-darwin"]
        self.assertEqual(x64["gui_runner"], json.loads(gui))

    def test_the_mac_x64_pool_never_takes_the_launch_jobs(self):
        # The MacBook denies Accessibility & co. to its jobs (no prompt): even
        # named by RUNNER_MACOS_X64_GUI, the launch jobs go to the image.
        for named in (POOLS["RUNNER_MACOS_X64"], "denext-macos-x64"):
            m = self.plan(MACOS_X64_ON_ARM="1", RUNNER_MACOS_ARM_SLOTS="4", RUNNER_MACOS_X64_GUI=named)
            x64 = m["x86_64-apple-darwin"]
            self.assertEqual(x64["gui_runner"], "macos-15-intel", named)
            self.assertEqual(x64["runs_on"]["package"], "macos-15-intel", named)
            self.assertEqual(x64["runs_on"]["e2e_fork"], "macos-15-intel", named)

    def test_x64_launch_jobs_on_the_arm_pool(self):
        # MACOS_X64_GUI_ON_ARM: the Tart guests (every privacy permission
        # pre-granted) run x86_64's package and e2e_fork under Rosetta 2.
        m = self.plan(MACOS_X64_GUI_ON_ARM="1", RUNNER_MACOS_ARM_SLOTS="4", RUNNER_MACOS_X64_GUI=POOLS["RUNNER_MACOS_X64"])
        x64 = m["x86_64-apple-darwin"]
        self.assertEqual(x64["gui_runner"], ARM)
        self.assertEqual(x64["runs_on"]["package"], ARM)
        self.assertEqual(x64["runs_on"]["e2e_fork"], ARM)
        # Off (or no arm64 pool): unchanged.
        self.assertEqual(self.plan(MACOS_X64_GUI_ON_ARM="0")["x86_64-apple-darwin"]["gui_runner"], "macos-15-intel")

    def test_no_x64_pool_means_no_fallback(self):
        env = {"TARGETS": "all", "RUNNER_MACOS_ARM": POOLS["RUNNER_MACOS_ARM"],
               "MACOS_X64_ON_ARM": "1", "RUNNER_MACOS_ARM_SLOTS": "2", "MACOS_CAP": "99"}
        x64 = by_target(plan_matrix.plan(env))["x86_64-apple-darwin"]
        self.assertNotIn("runner_fallback", x64)
        self.assertEqual(on_pool(x64), [])

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
        # And passes its slot to package and on to e2e_fork (launched there).
        lx = self.plan(RUNNER_LINUX_X64_2204=json.dumps(L2204))["x86_64-unknown-linux-gnu"]
        self.assertEqual(lx["runs_on"]["laufey"], L2204)
        self.assertEqual(on_pool(lx), ["e2e_fork", "laufey", "package"])

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
        # off sys.path: it must need no sibling module.
        out = subprocess.run(
            [sys.executable, "-I", os.path.join(HERE, "plan_matrix.py")],
            env={"TARGETS": "x86_64-apple-darwin", **POOLS},
            capture_output=True, text=True, check=True,
        ).stdout
        self.assertEqual(json.loads(out)[0]["runs_on"]["cli"], X64)


class PoolFree(unittest.TestCase):
    """POOL_FREE (the shared picker's `free` output) before the health and
    the slot variables."""

    def plan(self, **env):
        return by_target(plan_matrix.plan({"TARGETS": "all", **POOLS, **env}))

    def test_pool_free_drives_allocation(self):
        # Two free slots: denort and denort bin both start on the pool,
        # whatever the slot variable says.
        x64 = self.plan(
            POOL_FREE=json.dumps({"denext-macos-x64": 2}), RUNNER_MACOS_X64_SLOTS="1"
        )["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort", "denort_bin"])

    def test_no_free_slot_overflows(self):
        for n in (0, -1):
            x64 = self.plan(POOL_FREE=json.dumps({"denext-macos-x64": n}))["x86_64-apple-darwin"]
            self.assertEqual(on_pool(x64), [], n)

    def test_pool_free_wins_over_the_health(self):
        h = health({"denext-macos-x64": entry(slots=1, busy=1, healthy_slots=1)})
        x64 = self.plan(
            POOL_HEALTH=h, POOL_FREE=json.dumps({"denext-macos-x64": 1})
        )["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort_bin"])
        h = health({"denext-macos-x64": entry(slots=3, busy=0, healthy_slots=3)})
        x64 = self.plan(
            POOL_HEALTH=h, POOL_FREE=json.dumps({"denext-macos-x64": 0})
        )["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), [])

    def test_unparsable_or_unlisted_falls_back(self):
        # The slot variable then counts (RUNNER_MACOS_X64_SLOTS unset: 1).
        for v in ("", "not json", "[]", "3", json.dumps({"denext-linux-x64": 0}),
                  json.dumps({"denext-macos-x64": "0"}), json.dumps({"denext-macos-x64": True})):
            x64 = self.plan(POOL_FREE=v)["x86_64-apple-darwin"]
            self.assertEqual(on_pool(x64), ["cli", "denort_bin"], v)

    def test_unlisted_label_uses_the_health(self):
        h = health({"denext-macos-x64": entry(slots=1, busy=1, healthy_slots=1)})
        x64 = self.plan(
            POOL_HEALTH=h, POOL_FREE=json.dumps({"denext-macos-arm64": 2})
        )["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), [])

    def test_arm_full_and_x64_free_sends_the_x64_builds_to_mac_x64(self):
        m = self.plan(
            MACOS_X64_ON_ARM="1",
            POOL_FREE=json.dumps({"denext-macos-arm64": 0, "denext-macos-x64": 2}),
        )
        x64 = m["x86_64-apple-darwin"]
        self.assertTrue(x64["rosetta"])
        for job in ("denort", "denort_bin", "cli"):
            self.assertEqual(x64["runs_on"][job], X64, job)
        # Then GitHub: the jobs that launch the app (no GUI on mac-x64).
        self.assertEqual(x64["runs_on"]["package"], "macos-15-intel")
        arm = m["aarch64-apple-darwin"]
        self.assertEqual(arm["runs_on"]["denort"], "macos-15")
        self.assertEqual(arm["runs_on"]["denort_bin"], "macos-15")

    def test_arm_full_and_one_x64_slot(self):
        m = self.plan(
            MACOS_X64_ON_ARM="1",
            POOL_FREE=json.dumps({"denext-macos-arm64": 0, "denext-macos-x64": 1}),
        )
        x64 = m["x86_64-apple-darwin"]
        self.assertEqual(x64["runs_on"]["denort_bin"], X64)
        self.assertEqual(x64["runs_on"]["cli"], X64)
        self.assertEqual(x64["runs_on"]["denort"], "macos-15-intel")

    def test_laufey_by_pool_free(self):
        env = {"RUNNER_LINUX_X64_2204": json.dumps(L2204)}
        lx = self.plan(POOL_FREE=json.dumps({"denext-linux-x64-2204": 0}), **env)
        self.assertEqual(on_pool(lx["x86_64-unknown-linux-gnu"]), [])
        lx = self.plan(POOL_FREE=json.dumps({"denext-linux-x64-2204": 1}), **env)
        # one slot: laufey, then package and e2e_fork in it
        self.assertEqual(on_pool(lx["x86_64-unknown-linux-gnu"]), ["e2e_fork", "laufey", "package"])


class MacosCap(unittest.TestCase):
    """At most MACOS_CAP (default 3) macOS jobs OVERFLOW to GitHub's images (a
    leg with a pool that had no free slot); past it a macOS job waits for a
    pool that is merely busy (as the shared picker's macos-cap). Pool-less
    legs take the image and never count; a blocked pool is never waited on."""

    FULL = json.dumps({"denext-macos-arm64": 0, "denext-macos-x64": 0})

    def plan(self, **env):
        base = {k: v for k, v in POOLS.items() if k != "MACOS_CAP"}
        return by_target(plan_matrix.plan({"TARGETS": "all", **base, **env}))

    @staticmethod
    def hosted(m):
        return sum(
            1 for t in m.values() if t["os"].startswith("macos")
            for r in t["runs_on"].values() if r == t["os"]
        )

    def test_default_cap_is_three(self):
        m = self.plan(POOL_FREE=self.FULL)
        arm, x64 = m["aarch64-apple-darwin"], m["x86_64-apple-darwin"]
        # The first three overflows go to GitHub: arm64's denort and denort
        # bin, and x86_64's denort bin (the longest chain first).
        self.assertEqual(arm["runs_on"]["denort"], "macos-15")
        self.assertEqual(arm["runs_on"]["denort_bin"], "macos-15")
        self.assertEqual(x64["runs_on"]["denort_bin"], "macos-15-intel")
        # The rest wait for their (merely busy) pool.
        self.assertEqual(on_pool(arm), ["cli", "e2e_fork", "package"])
        self.assertEqual(on_pool(x64), ["cli", "denort"])
        # Pool-less legs take the image and do not count: both laufey legs
        # and x86_64's package and e2e_fork (no GUI session on mac-x64).
        self.assertEqual(arm["runs_on"]["laufey"], "macos-15")
        self.assertEqual(x64["runs_on"]["laufey"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["package"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["e2e_fork"], "macos-15-intel")
        self.assertEqual(self.hosted(m), 7)  # 3 overflows + 4 pool-less

    def test_garbage_cap_is_three(self):
        want = self.plan(POOL_FREE=self.FULL)
        for v in ("x", "", "-"):
            self.assertEqual(self.plan(POOL_FREE=self.FULL, MACOS_CAP=v), want, v)

    def test_cap_zero_waits_for_every_pool(self):
        m = self.plan(POOL_FREE=self.FULL, MACOS_CAP="0")
        self.assertEqual(on_pool(m["aarch64-apple-darwin"]),
                         ["cli", "denort", "denort_bin", "e2e_fork", "package"])
        x64 = m["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort", "denort_bin"])
        self.assertEqual(x64["runs_on"]["package"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["e2e_fork"], "macos-15-intel")

    def test_a_high_cap_overflows_everything(self):
        m = self.plan(POOL_FREE=self.FULL, MACOS_CAP="99")
        for name in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
            self.assertEqual(on_pool(m[name]), [], name)

    def test_free_slots_come_first(self):
        free = json.dumps({"denext-macos-arm64": 2, "denext-macos-x64": 2})
        self.assertEqual(
            self.plan(POOL_FREE=free, MACOS_CAP="0"), self.plan(POOL_FREE=free, MACOS_CAP="99")
        )

    def test_no_pool_means_the_image_whatever_the_cap(self):
        m = by_target(plan_matrix.plan({"TARGETS": "all", "MACOS_CAP": "0"}))
        for t in m.values():
            self.assertEqual(set(t["runs_on"].values()), {t["os"]}, t["target"])

    def test_other_targets_never_count(self):
        # Linux laufey overflowing does not spend the macOS share.
        env = {"RUNNER_LINUX_X64_2204": json.dumps(L2204), "MACOS_CAP": "1",
               "POOL_FREE": json.dumps({"denext-linux-x64-2204": 0, "denext-macos-x64": 0})}
        m = by_target(plan_matrix.plan({
            "TARGETS": "x86_64-unknown-linux-gnu,x86_64-apple-darwin",
            "RUNNER_MACOS_X64": POOLS["RUNNER_MACOS_X64"], **env}))
        self.assertEqual(on_pool(m["x86_64-unknown-linux-gnu"]), [])
        x64 = m["x86_64-apple-darwin"]
        # x86_64's laufey has no pool (the image, not counted); one overflow
        # (denort bin), then denort and cli wait for mac-x64.
        self.assertEqual(x64["runs_on"]["laufey"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["denort_bin"], "macos-15-intel")
        self.assertEqual(on_pool(x64), ["cli", "denort"])

    def test_run_43756144777_busy_mac_x64_two_x86_legs_go_to_github(self):
        # denext.24 runtime run 43756144777: RUNNER_MACOS_ARM names an image (the
        # arm64 legs are pool-less), mac-x64 is busy 1/1 with another run's jobs
        # queued on it (the picker's free: 0). Before the fix the three pool-less
        # arm64 legs spent the cap and both x86_64 denort legs were forced onto
        # mac-x64; now both take GitHub's image.
        m = by_target(plan_matrix.plan({
            "TARGETS": "all", "RUNNER_MACOS_ARM": "macos-latest",
            "RUNNER_MACOS_X64": POOLS["RUNNER_MACOS_X64"],
            "POOL_FREE": json.dumps({"denext-macos-x64": 0}), "MACOS_CAP": "3"}))
        x64 = m["x86_64-apple-darwin"]
        self.assertEqual(x64["runs_on"]["denort"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["denort_bin"], "macos-15-intel")
        self.assertEqual(on_pool(m["aarch64-apple-darwin"]), [])

    def test_pool_less_legs_never_spend_the_cap(self):
        m = by_target(plan_matrix.plan({
            "TARGETS": "all", "RUNNER_MACOS_ARM": "macos-latest",
            "RUNNER_MACOS_X64": POOLS["RUNNER_MACOS_X64"],
            "POOL_FREE": json.dumps({"denext-macos-x64": 0}), "MACOS_CAP": "2"}))
        x64 = m["x86_64-apple-darwin"]
        # the two overflows the cap allows are x86_64's own build legs
        self.assertEqual(x64["runs_on"]["denort_bin"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["denort"], "macos-15-intel")
        self.assertEqual(on_pool(x64), ["cli"])

    def test_a_blocked_pool_is_never_waited_on(self):
        # attempt 2 of that run: mac-x64 stalled in attempt 1 (POOL_BLOCKED).
        for cap in ("0", "3"):
            m = by_target(plan_matrix.plan({
                "TARGETS": "all", "RUNNER_MACOS_ARM": POOLS["RUNNER_MACOS_ARM"],
                "RUNNER_MACOS_X64": POOLS["RUNNER_MACOS_X64"],
                "POOL_FREE": json.dumps({"denext-macos-arm64": 0, "denext-macos-x64": 0}),
                "POOL_BLOCKED": json.dumps(["denext-macos-x64"]), "MACOS_CAP": cap,
                "MACOS_X64_GUI_ON_ARM": "0"}))
            self.assertEqual(on_pool(m["x86_64-apple-darwin"]), [], cap)

    def test_garbage_blocked_is_ignored(self):
        want = self.plan(POOL_FREE=self.FULL)
        for v in ("x", "{}", "[1]"):
            self.assertEqual(self.plan(POOL_FREE=self.FULL, POOL_BLOCKED=v), want, v)


# Every pool variable that exists in the repository (2026-11-25).
OURS = {
    **POOLS,
    "RUNNER_LINUX_X64_2204": json.dumps(L2204),
    "RUNNER_LINUX_ARM64": '["self-hosted","denext-linux-arm64"]',
    "RUNNER_WINDOWS_X64": '["self-hosted","denext-windows-x64"]',
    "RUNNER_WINDOWS_ARM": '["self-hosted","denext-windows-arm64"]',
    "RUNNER_MACOS_ARM_SLOTS": "2",
    "RUNNER_WINDOWS_X64_SLOTS": "2",
    "RUNNER_WINDOWS_ARM_SLOTS": "2",
    "RUNNER_LINUX_X64_2204_SLOTS": "3",
    "RUNNER_LINUX_ARM64_SLOTS": "1",
}
LARM = ["self-hosted", "denext-linux-arm64"]
WX64 = ["self-hosted", "denext-windows-x64"]
WARM = ["self-hosted", "denext-windows-arm64"]
LINUX_WINDOWS = (
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
)


class LinuxWindowsPools(unittest.TestCase):
    """The Linux and Windows pools: laufey and the launch jobs, never the
    release builds (memory, swapfile, sysroot mount, chroot) nor the x64
    Windows package job (its per-machine .msi install needs an elevated
    administrator, which only the win-arm64 pool's job account is)."""

    def plan(self, **env):
        return by_target(plan_matrix.plan({"TARGETS": "all", **OURS, **env}))

    def test_routing_with_every_pool(self):
        m = self.plan()
        want = {
            "x86_64-unknown-linux-gnu": (L2204, L2204, L2204),
            "aarch64-unknown-linux-gnu": (LARM, LARM, LARM),
            "x86_64-pc-windows-msvc": (WX64, "windows-2025", WX64),
            "aarch64-pc-windows-msvc": (WARM, WARM, WARM),
        }
        for name, (laufey, package, e2e) in want.items():
            t = m[name]
            self.assertEqual(t["laufey_runner"], laufey, name)
            self.assertEqual(t["package_runner"], package, name)
            self.assertEqual(t["gui_runner"], e2e, name)
            self.assertEqual(t["runner"], t["os"], name)
            for job in ("denort", "denort_bin", "cli"):
                self.assertEqual(t["runs_on"][job], t["os"], (name, job))
            self.assertEqual(t["runs_on"]["laufey"], laufey, name)
            self.assertEqual(t["runs_on"]["package"], package, name)
            self.assertEqual(t["runs_on"]["e2e_fork"], e2e, name)

    def test_macos_laufey_stays_on_the_image(self):
        m = self.plan()
        for name in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
            self.assertEqual(m[name]["laufey_runner"], m[name]["os"], name)
            self.assertEqual(m[name]["runs_on"]["laufey"], m[name]["os"], name)

    def test_without_the_variables_every_image(self):
        m = by_target(plan_matrix.plan({"TARGETS": "all", **POOLS}))
        for name in LINUX_WINDOWS:
            t = m[name]
            for field in ("runner", "gui_runner", "package_runner"):
                self.assertEqual(t[field], t["os"], (name, field))
            self.assertEqual(set(t["runs_on"].values()), {t["os"]}, name)

    def test_the_linux_package_chain_shares_one_slot(self):
        # laufey -> package -> e2e_fork pass the single linux-arm64 slot on.
        arm = self.plan(POOL_FREE=json.dumps({"denext-linux-arm64": 1}))["aarch64-unknown-linux-gnu"]
        self.assertEqual(on_pool(arm), ["e2e_fork", "laufey", "package"])

    def test_full_or_unhealthy_pools_overflow_to_the_image(self):
        free = json.dumps({
            "denext-linux-x64-2204": 0, "denext-linux-arm64": 0,
            "denext-windows-x64": 0, "denext-windows-arm64": 0,
        })
        m = self.plan(POOL_FREE=free)
        for name in LINUX_WINDOWS:
            self.assertEqual(on_pool(m[name]), [], name)
        h = health({
            "denext-windows-x64": entry(slots=2, ok=False),
            "denext-windows-arm64": entry(slots=2, paused=True),
            "denext-linux-arm64": entry(slots=1, busy=1, healthy_slots=1),
        })
        m = self.plan(POOL_HEALTH=h)
        for name in ("aarch64-unknown-linux-gnu", "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"):
            self.assertEqual(on_pool(m[name]), [], name)
        self.assertEqual(on_pool(m["x86_64-unknown-linux-gnu"]), ["e2e_fork", "laufey", "package"])

    def test_one_windows_slot_goes_to_laufey_first(self):
        # laufey starts after setup; e2e_fork (after package on the image and
        # cli) finds no slot left in this plan and takes the image.
        win = self.plan(POOL_FREE=json.dumps({"denext-windows-x64": 1}))["x86_64-pc-windows-msvc"]
        self.assertEqual(on_pool(win), ["laufey"])
        win = self.plan(POOL_FREE=json.dumps({"denext-windows-x64": 2}))["x86_64-pc-windows-msvc"]
        self.assertEqual(on_pool(win), ["e2e_fork", "laufey"])

    def test_slot_variables(self):
        win = self.plan(RUNNER_WINDOWS_ARM_SLOTS="0")["aarch64-pc-windows-msvc"]
        self.assertEqual(on_pool(win), [])
        # One slot (the default for a garbage value): laufey -> package ->
        # e2e_fork pass it on, as on the Linux arm64 pool.
        win = self.plan(RUNNER_WINDOWS_ARM_SLOTS="x")["aarch64-pc-windows-msvc"]
        self.assertEqual(on_pool(win), ["e2e_fork", "laufey", "package"])

    def test_gui_variables_override_the_launch_jobs_only(self):
        other = '["self-hosted","denext-linux-arm64-desktop"]'
        m = self.plan(RUNNER_LINUX_ARM64_GUI=other, RUNNER_LINUX_X64_GUI="ubuntu-22.04",
                      RUNNER_WINDOWS_X64_GUI="windows-2025", RUNNER_WINDOWS_ARM_GUI="win-gui")
        arm = m["aarch64-unknown-linux-gnu"]
        self.assertEqual(arm["gui_runner"], json.loads(other))
        self.assertEqual(arm["package_runner"], json.loads(other))
        self.assertEqual(arm["runs_on"]["package"], json.loads(other))
        self.assertEqual(arm["laufey_runner"], LARM)
        # An image name turns the pool off for the launch jobs.
        lx = m["x86_64-unknown-linux-gnu"]
        self.assertEqual(lx["runs_on"]["package"], "ubuntu-22.04")
        self.assertEqual(lx["runs_on"]["e2e_fork"], "ubuntu-22.04")
        self.assertEqual(lx["runs_on"]["laufey"], L2204)
        self.assertEqual(m["x86_64-pc-windows-msvc"]["runs_on"]["e2e_fork"], "windows-2025")
        warm = m["aarch64-pc-windows-msvc"]
        self.assertEqual(warm["runs_on"]["e2e_fork"], "win-gui")
        # The arm64 Windows package job follows the GUI pool (win-arm64's job
        # account is an elevated administrator) ...
        self.assertEqual(warm["package_runner"], "win-gui")
        self.assertEqual(warm["runs_on"]["package"], "win-gui")
        # ... the x64 one stays on the image whatever is named.
        wx = self.plan(RUNNER_WINDOWS_X64_GUI='["self-hosted","denext-windows-x64"]')["x86_64-pc-windows-msvc"]
        self.assertEqual(wx["package_runner"], "windows-2025")
        self.assertEqual(wx["runs_on"]["package"], "windows-2025")
        self.assertEqual(wx["runs_on"]["e2e_fork"], WX64)

    def test_the_arm64_windows_package_job_can_be_sent_to_the_image(self):
        # An image name in RUNNER_WINDOWS_ARM_GUI turns the pool off for it.
        warm = self.plan(RUNNER_WINDOWS_ARM_GUI="windows-11-arm")["aarch64-pc-windows-msvc"]
        self.assertEqual(warm["package_runner"], "windows-11-arm")
        self.assertEqual(warm["runs_on"]["package"], "windows-11-arm")
        self.assertEqual(warm["runs_on"]["laufey"], WARM)

    def test_linux_and_windows_never_wait_for_a_pool(self):
        # The macOS cap is for macOS: a full Linux or Windows pool sends its
        # legs to the image even at MACOS_CAP=0.
        free = json.dumps({"denext-windows-x64": 0, "denext-linux-arm64": 0})
        m = self.plan(POOL_FREE=free, MACOS_CAP="0")
        self.assertEqual(on_pool(m["x86_64-pc-windows-msvc"]), [])
        self.assertEqual(on_pool(m["aarch64-unknown-linux-gnu"]), [])

    def test_a_reused_laufey_still_launches_on_the_pool(self):
        lx = self.plan(SKIP_JOBS="laufey denort")["x86_64-unknown-linux-gnu"]
        self.assertEqual(on_pool(lx), ["e2e_fork", "package"])


class GuiOnArm(unittest.TestCase):
    """x86_64-apple-darwin's launch jobs on the arm64 GUI pool under Rosetta
    2: the default whenever that pool exists (MACOS_X64_GUI_ON_ARM off turns
    it off)."""

    def plan(self, **env):
        base = {k: v for k, v in POOLS.items() if k != "MACOS_X64_GUI_ON_ARM"}
        return by_target(plan_matrix.plan({"TARGETS": "all", **base, **env}))

    def test_on_by_default(self):
        x64 = self.plan(RUNNER_MACOS_ARM_SLOTS="4")["x86_64-apple-darwin"]
        self.assertEqual(x64["gui_runner"], ARM)
        self.assertEqual(x64["package_runner"], ARM)
        self.assertEqual(x64["runs_on"]["package"], ARM)
        self.assertEqual(x64["runs_on"]["e2e_fork"], ARM)
        # The build jobs are unchanged (MACOS_X64_ON_ARM is their switch).
        self.assertFalse(x64["rosetta"])
        self.assertEqual(x64["runner"], X64)

    def test_off_spellings(self):
        for off in ("0", "false", "OFF", " no "):
            x64 = self.plan(MACOS_X64_GUI_ON_ARM=off)["x86_64-apple-darwin"]
            self.assertEqual(x64["gui_runner"], "macos-15-intel", off)
        for on in ("", "1", "true", "maybe"):
            x64 = self.plan(MACOS_X64_GUI_ON_ARM=on)["x86_64-apple-darwin"]
            self.assertEqual(x64["gui_runner"], ARM, on)

    def test_no_arm_pool_is_the_image(self):
        for arm in ("", "macos-latest", "macos-15"):
            x64 = self.plan(RUNNER_MACOS_ARM=arm)["x86_64-apple-darwin"]
            self.assertEqual(x64["gui_runner"], "macos-15-intel", arm)

    def test_the_arm_gui_variable_is_followed(self):
        gui = '["self-hosted","denext-macos-arm64-gui"]'
        x64 = self.plan(RUNNER_MACOS_ARM_GUI=gui)["x86_64-apple-darwin"]
        self.assertEqual(x64["gui_runner"], json.loads(gui))

    def test_a_named_x64_gui_pool_wins_over_the_default(self):
        x64 = self.plan(RUNNER_MACOS_X64_GUI="some-gui-label")["x86_64-apple-darwin"]
        self.assertEqual(x64["gui_runner"], "some-gui-label")
        # ... but not over an explicit on.
        x64 = self.plan(RUNNER_MACOS_X64_GUI="some-gui-label", MACOS_X64_GUI_ON_ARM="1")["x86_64-apple-darwin"]
        self.assertEqual(x64["gui_runner"], ARM)

    def test_the_mac_x64_pool_named_still_means_the_arm_pool(self):
        for named in (POOLS["RUNNER_MACOS_X64"], "denext-macos-x64"):
            x64 = self.plan(RUNNER_MACOS_X64_GUI=named)["x86_64-apple-darwin"]
            self.assertEqual(x64["gui_runner"], ARM, named)
            x64 = self.plan(RUNNER_MACOS_X64_GUI=named, MACOS_X64_GUI_ON_ARM="0")["x86_64-apple-darwin"]
            self.assertEqual(x64["gui_runner"], "macos-15-intel", named)

    def test_the_launch_jobs_follow_the_x64_build_slot_on_the_arm_pool(self):
        # With the build on the arm64 pool too (MACOS_X64_ON_ARM), package
        # takes over denort's slot there and e2e_fork package's.
        x64 = self.plan(MACOS_X64_ON_ARM="1", RUNNER_MACOS_ARM_SLOTS="4")["x86_64-apple-darwin"]
        self.assertEqual(on_pool(x64), ["cli", "denort", "denort_bin", "e2e_fork", "package"])
        for job in ("denort", "denort_bin", "cli", "package", "e2e_fork"):
            self.assertEqual(x64["runs_on"][job], ARM, job)

    def test_past_the_cap_waits_on_the_arm_pool_but_never_a_blocked_one(self):
        full = json.dumps({"denext-macos-arm64": 0, "denext-macos-x64": 0})
        x64 = self.plan(POOL_FREE=full, MACOS_CAP="0")["x86_64-apple-darwin"]
        self.assertEqual(x64["runs_on"]["package"], ARM)
        self.assertEqual(x64["runs_on"]["e2e_fork"], ARM)
        x64 = self.plan(POOL_FREE=full, MACOS_CAP="0",
                        POOL_BLOCKED=json.dumps(["denext-macos-arm64"]))["x86_64-apple-darwin"]
        self.assertEqual(x64["runs_on"]["package"], "macos-15-intel")
        self.assertEqual(x64["runs_on"]["e2e_fork"], "macos-15-intel")


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
        for job, field in (("package", "package_runner"), ("e2e_fork", "gui_runner")):
            r = runs_on(job_block(self.text, job))
            self.assertIn(f"matrix.{field}", r, job)
            self.assertNotIn("matrix.runner ", r, job)

    def test_the_plan_step_passes_the_linux_and_windows_pools(self):
        setup = job_block(self.text, "setup")
        for var in ("RUNNER_LINUX_ARM64", "RUNNER_WINDOWS_X64", "RUNNER_WINDOWS_ARM",
                    "RUNNER_LINUX_X64_GUI", "RUNNER_LINUX_ARM64_GUI",
                    "RUNNER_WINDOWS_X64_GUI", "RUNNER_WINDOWS_ARM_GUI",
                    "MACOS_X64_GUI_ON_ARM"):
            self.assertIn(f"{var}: ${{{{ vars.{var} }}}}", setup)

    def test_laufey_on_windows_works_without_chocolatey(self):
        # Our Windows pools have no Chocolatey: the job unpacks the same
        # pinned packages itself and puts make and ninja first on PATH.
        job = job_block(self.text, "laufey")
        step = job.split("- name: Install dependencies (Windows)\n", 1)[1].split("\n      - ", 1)[0]
        self.assertIn("if command -v choco >/dev/null 2>&1; then", step)
        self.assertIn("nupkg make 4.4.1 8b2dec73ed4c84163c4a4f9bfe658553b1e3d1fdfa8930d2aa438e030ff21f9d", step)
        self.assertIn("nupkg ninja 1.13.2 b44ad9d0ddef594489a75b04be5bf77f50a7d92cf8fab88ea2f9f7c2a399919b", step)
        self.assertIn('| sha256sum -c -', step)
        self.assertIn('>>"$GITHUB_PATH"', step)
        self.assertIn("choco_install make --version=4.4.1", step)
        self.assertIn("choco_install ninja --version=1.13.2", step)

    def test_rpm_smoke_stays_on_the_image(self):
        # It runs `docker run -v <the job's files>`: no pool of ours can. It
        # reads `runner`, which plan_matrix keeps the image on Linux.
        self.assertIn("|| matrix.runner || matrix.os }}", runs_on(job_block(self.text, "rpm-smoke")))
        env = {"TARGETS": "all", **OURS}
        for t in plan_matrix.plan(env):
            if "-linux-" in t["target"]:
                self.assertEqual(t["runner"], t["os"], t["target"])

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
        self.assertNotIn("pick_runner", setup)
        self.assertIn("POOL_FREE: ${{ steps.pools.outputs.free }}", setup)
        self.assertIn("POOL_BLOCKED: ${{ steps.pools.outputs.blocked }}", setup)

    def test_setup_is_named_as_a_picker(self):
        # The shared action's settle (in a sibling run) waits for picker jobs by
        # name: `pick … runners`, `<leg> / pick … runners`, `<job>: pick … runners`.
        # setup's `pools` step picks the matrix's slots, so its name must match.
        setup = job_block(self.text, "setup")
        m = re.search(r'^    name: ["\']?([^"\'\n]+)["\']?$', setup, re.M)
        self.assertIsNotNone(m)
        self.assertRegex(m.group(1), r"(^| / |: )pick [^/]*runners?$")
        self.assertIn("uses: Brainwires/.github/actions/pick-runners@", setup)
        self.assertIn("MACOS_CAP: '3'", setup)
        self.assertIn("POOL_HEALTH: ${{ vars.DENEXT_POOL_HEALTH }}", setup)
        self.assertIn("SKIP_JOBS:", setup)
        for _, slots_var in plan_matrix.SLOT_VARS:
            self.assertIn(f"{slots_var}: ${{{{ vars.{slots_var} ||", setup)

    def test_the_pick_covers_every_pool_plan_matrix_uses(self):
        # The setup job's pick-runners step lists each pool of SLOT_VARS, so
        # its `free` output (POOL_FREE) has every label plan_matrix asks for.
        setup = job_block(self.text, "setup")
        step = setup.split("      - id: pools\n", 1)[1].split("      - id: plan\n", 1)[0]
        self.assertRegex(
            step, r"uses: Brainwires/\.github/actions/pick-runners@[0-9a-f]{40} # pick-runners"
        )
        self.assertIn("vars: ${{ toJSON(vars) }}", step)
        pools = re.search(r"jobs: (.*)$", step, re.M).group(1).split()
        names = sorted(e.split("=", 1)[1] for e in pools)
        want = {
            "RUNNER_MACOS_ARM": "macos-arm64",
            "RUNNER_MACOS_X64": "macos-x64",
            "RUNNER_LINUX_X64_2204": "linux-x64-2204",
            "RUNNER_LINUX_ARM64": "linux-arm64",
            "RUNNER_WINDOWS_X64": "windows-x64",
            "RUNNER_WINDOWS_ARM": "windows-arm64",
        }
        self.assertEqual(names, sorted(want[var] for var, _ in plan_matrix.SLOT_VARS))
        # Before the plan step reads it.
        self.assertLess(setup.index("- id: pools"), setup.index("- id: plan"))

    def test_the_linux_jobs_take_the_picker(self):
        runners = job_block(self.text, "runners")
        self.assertIn("    name: pick runners\n", runners)
        self.assertIn("    runs-on: ubuntu-latest\n", runners)
        for job, key in (("guard", "guard"), ("setup", "setup"),
                         ("e2e-harness", "harness"), ("publish", "publish")):
            block = job_block(self.text, job)
            self.assertIn(
                f"runs-on: ${{{{ fromJSON(needs.runners.outputs.plan || '{{}}').{key} || 'ubuntu-24.04' }}}}",
                block, job,
            )
            self.assertRegex(block, r"needs: (runners|\[runners, )", job)
            self.assertIn(f" {key}=", " " + re.search(r"jobs: (.*)$", runners, re.M).group(1), job)
        self.assertIsNone(re.search(r"vars\.RUNNER_LINUX_X64(?![_A-Z0-9])", self.text))


if __name__ == "__main__":
    unittest.main()
