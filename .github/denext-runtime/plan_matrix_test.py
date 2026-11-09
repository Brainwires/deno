#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests of plan_matrix.py and of the denext_runtime.yml jobs that use it.
Run: python3 -I .github/denext-runtime/plan_matrix_test.py"""
import os
import re
import sys
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


def by_target(matrix):
    return {t["target"]: t for t in matrix}


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

    def test_target_selection(self):
        m = plan_matrix.plan({"TARGETS": " x86_64-apple-darwin ,", **POOLS})
        self.assertEqual([t["target"] for t in m], ["x86_64-apple-darwin"])
        with self.assertRaises(SystemExit):
            plan_matrix.plan({"TARGETS": "x86_64-apple-ios"})


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
        for var in ("RUNNER_MACOS_ARM_GUI", "RUNNER_MACOS_X64_GUI"):
            self.assertIn(f"{var}: ${{{{ vars.{var} }}}}", setup)

    def test_jobs_that_launch_the_app_use_the_gui_runner(self):
        for job in ("package", "e2e_fork"):
            r = runs_on(job_block(self.text, job))
            self.assertIn("matrix.gui_runner", r, job)
            self.assertNotIn("matrix.runner ", r, job)

    def test_build_jobs_keep_the_build_runner(self):
        for job in ("denort", "denort_bin", "cli"):
            r = runs_on(job_block(self.text, job))
            self.assertIn("matrix.runner", r, job)
            self.assertNotIn("gui_runner", r, job)


if __name__ == "__main__":
    unittest.main()
