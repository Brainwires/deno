#!/usr/bin/env python3
"""Tests for release_gate.py: the tag gate's rule (the newest full run is
green, and no run of the commit has a failed job that was not re-run green)
and the "Latest" release choice.

Usage: python3 -I .github/denext-runtime/release_gate_test.py
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import release_gate as g  # noqa: E402


def jobs(conclusion="success", attempt=1, legs=g.LEGS, failed=()):
    out = [{"name": "lint / ubuntu-22.04", "conclusion": conclusion if not failed else "success", "run_attempt": attempt}]
    for leg in legs:
        for job in ("build debug", "test libs"):
            name = f"{leg} / {job}"
            out.append({"name": name, "conclusion": "failure" if name in failed else conclusion, "run_attempt": attempt})
    return out


def linux_only(conclusion="success"):
    out = jobs(conclusion, legs=("ubuntu-22.04",))
    # The other legs' reusable calls are skipped as a whole.
    out += [{"name": leg, "conclusion": "skipped", "run_attempt": 1} for leg in g.LEGS if leg != "ubuntu-22.04"]
    return out


def run(id_, created, conclusion="success", event="push", status="completed", attempt=1, job_list=None):
    return {
        "id": id_,
        "event": event,
        "head_branch": "development-denext",
        "status": status,
        "conclusion": conclusion,
        "run_attempt": attempt,
        "created_at": created,
        "jobs": jobs() if job_list is None else job_list,
    }


class TestsGate(unittest.TestCase):
    def test_one_green_full_run_passes(self):
        errors, full, retried = g.evaluate([run(1, "2026-10-25T10:00:00Z")])
        self.assertEqual(errors, [])
        self.assertEqual(full, 1)
        self.assertEqual(retried, [])

    def test_a_linux_only_run_is_not_full(self):
        errors, full, _ = g.evaluate([run(1, "2026-10-25T10:00:00Z", job_list=linux_only())])
        self.assertIsNone(full)
        self.assertTrue(any("no full" in e for e in errors))

    def test_a_missing_leg_is_not_full(self):
        legs = tuple(leg for leg in g.LEGS if leg != "windows-11-arm")
        errors, full, _ = g.evaluate([run(1, "2026-10-25T10:00:00Z", job_list=jobs(legs=legs))])
        self.assertIsNone(full)
        self.assertTrue(errors)

    def test_a_failed_push_run_is_not_cleared_by_a_green_dispatch(self):
        # rc.6 (08e6d7e46): the push run's arm64 lib tests aborted, a dispatch passed.
        failed = run(
            1, "2026-10-25T16:31:00Z", "failure",
            job_list=jobs(failed=("ubuntu-22.04-arm / test libs",)),
        )
        green = run(2, "2026-10-25T16:32:00Z", event="workflow_dispatch")
        errors, full, _ = g.evaluate([green, failed])
        self.assertEqual(full, 2)
        self.assertEqual(len(errors), 1)
        self.assertIn("run 1", errors[0])
        self.assertIn("ubuntu-22.04-arm / test libs", errors[0])

    def test_a_failed_job_re_run_green_in_the_same_run_counts(self):
        first = jobs(failed=("macos-14 / test libs",))
        retry = [{"name": "macos-14 / test libs", "conclusion": "success", "run_attempt": 2}]
        errors, full, retried = g.evaluate([run(1, "2026-10-25T10:00:00Z", attempt=2, job_list=first + retry)])
        self.assertEqual(errors, [])
        self.assertEqual(full, 1)
        self.assertEqual(retried, ["macos-14 / test libs (run 1, passed on attempt 2)"])

    def test_a_re_run_that_failed_again_still_blocks(self):
        first = jobs(failed=("macos-14 / test libs",))
        retry = [{"name": "macos-14 / test libs", "conclusion": "failure", "run_attempt": 2}]
        errors, _, _ = g.evaluate([run(1, "2026-10-25T10:00:00Z", "failure", attempt=2, job_list=first + retry)])
        self.assertTrue(errors)

    def test_a_failed_branch_run_of_the_same_commit_blocks(self):
        failed = run(1, "2026-10-25T09:00:00Z", "failure", job_list=linux_only("failure"))
        errors, _, _ = g.evaluate([run(2, "2026-10-25T10:00:00Z"), failed])
        self.assertEqual(len(errors), 1)

    def test_a_failed_job_in_a_cancelled_run_blocks(self):
        cancelled = run(
            1, "2026-10-25T09:00:00Z", "cancelled",
            job_list=jobs(failed=("windows-2022 / test unit",)) + [
                {"name": "windows-2022 / test unit", "conclusion": "failure", "run_attempt": 1}
            ],
        )
        errors, _, _ = g.evaluate([run(2, "2026-10-25T10:00:00Z"), cancelled])
        self.assertEqual(len(errors), 1)

    def test_the_newest_full_run_must_be_green(self):
        older = run(1, "2026-10-25T09:00:00Z")
        newer = run(2, "2026-10-25T10:00:00Z", "cancelled", job_list=jobs("cancelled"))
        errors, full, _ = g.evaluate([older, newer])
        self.assertIsNone(full)
        self.assertTrue(any("newest full run, 2" in e for e in errors))

    def test_an_unfinished_run_blocks(self):
        errors, _, _ = g.evaluate([run(1, "2026-10-25T09:00:00Z"), run(2, "2026-10-25T10:00:00Z", None, status="in_progress")])
        self.assertTrue(any("has not finished" in e for e in errors))

    def test_timed_out_and_startup_failure_count_as_failures(self):
        for bad in ("timed_out", "startup_failure"):
            j = jobs()
            j[1]["conclusion"] = bad
            errors, _, _ = g.evaluate([run(1, "2026-10-25T10:00:00Z", "failure", job_list=j)])
            self.assertTrue(errors, bad)


class Latest(unittest.TestCase):
    def test_newest_rc(self):
        self.assertTrue(g.is_newest("3.0.0-rc.6", ["3.0.0-rc.5", "3.0.0-rc.1", "2.9.7-denext.22"]))

    def test_numeric_prerelease_order(self):
        self.assertTrue(g.is_newest("3.0.0-rc.10", ["3.0.0-rc.9", "3.0.0-rc.2"]))
        self.assertFalse(g.is_newest("3.0.0-rc.9", ["3.0.0-rc.10"]))

    def test_release_after_its_rcs(self):
        self.assertTrue(g.is_newest("3.0.0", ["3.0.0-rc.12"]))
        self.assertFalse(g.is_newest("3.0.0-rc.13", ["3.0.0"]))

    def test_an_older_line_is_not_latest(self):
        self.assertFalse(g.is_newest("2.9.7-denext.23", ["3.0.0-rc.6"]))

    def test_unparseable_others_are_ignored(self):
        self.assertTrue(g.is_newest("3.0.0-rc.6", ["nightly", "3.0.0-rc.5"]))

    def test_build_metadata_is_ignored(self):
        self.assertTrue(g.is_newest("3.0.0-rc.6", ["3.0.0-rc.6+ci.4"]))


if __name__ == "__main__":
    unittest.main()
