#!/usr/bin/env python3
"""Tests for tag_gate.py, denext_runtime.yml's release gate: a commit is
released only with a full green `denext tests` run (every OS leg, Windows
arm64 included), no failed run or attempt, nothing still running, and only
from development-denext or its release branch. The last cases run the script
against a stand-in `gh` that answers from canned API responses.

Usage: python3 -I .github/denext-runtime/tag_gate_test.py
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
GATE = os.path.join(HERE, "tag_gate.py")

spec = importlib.util.spec_from_file_location("tag_gate", GATE)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

ALL_LEGS = set(gate.REQUIRED_LEGS)
ON_DEV = {"development-denext": True, "rel/3.0.0-rc.7": False}


def run(id, conclusion="success", status="completed", attempts=()):
    return {
        "id": id,
        "status": status,
        "conclusion": conclusion,
        "attempts": list(attempts),
    }


def legs(table):
    return lambda run_id: table.get(run_id, set())


class Evaluate(unittest.TestCase):
    def test_full_green_run_passes(self):
        full, errors = gate.evaluate([run(7)], legs({7: ALL_LEGS}), ON_DEV)
        self.assertEqual(errors, [])
        self.assertEqual(full, 7)

    def test_a_red_run_beside_a_green_one_is_refused(self):
        # rc.6: a failed push run (arm64 SIGABRT), then a green dispatch.
        full, errors = gate.evaluate(
            [run(40212070372, "failure"), run(40212070967)],
            legs({40212070967: ALL_LEGS}),
            ON_DEV,
        )
        self.assertEqual(full, 40212070967)
        self.assertEqual(len(errors), 1)
        self.assertIn("40212070372 (attempt 1: failure)", errors[0])

    def test_a_failed_attempt_of_a_rerun_is_refused(self):
        # "Re-run failed jobs" turns the run green; its first attempt failed.
        full, errors = gate.evaluate(
            [run(9, attempts=["failure"])], legs({9: ALL_LEGS}), ON_DEV
        )
        self.assertEqual(full, 9)
        self.assertEqual(len(errors), 1)
        self.assertIn("9 (attempt 1: failure)", errors[0])

    def test_timeouts_and_startup_failures_are_red(self):
        for conclusion in ("timed_out", "startup_failure"):
            _, errors = gate.evaluate(
                [run(1, conclusion), run(2)], legs({2: ALL_LEGS}), ON_DEV
            )
            self.assertEqual(len(errors), 1, conclusion)
            self.assertIn(conclusion, errors[0])

    def test_a_cancelled_run_is_not_red(self):
        # Superseded by a newer push to the same branch.
        full, errors = gate.evaluate(
            [run(1, "cancelled"), run(2)], legs({2: ALL_LEGS}), ON_DEV
        )
        self.assertEqual((full, errors), (2, []))

    def test_a_run_in_progress_holds_the_release(self):
        _, errors = gate.evaluate(
            [run(1), run(2, None, "in_progress")],
            legs({1: ALL_LEGS}),
            ON_DEV,
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("in progress", errors[0])
        self.assertIn("2", errors[0])

    def test_windows_arm64_is_a_required_leg(self):
        self.assertIn("windows-11-arm", gate.REQUIRED_LEGS)
        full, errors = gate.evaluate(
            [run(3)], legs({3: ALL_LEGS - {"windows-11-arm"}}), ON_DEV
        )
        self.assertIsNone(full)
        self.assertEqual(len(errors), 1)
        self.assertIn("no successful full", errors[0])

    def test_a_linux_only_run_never_vouches(self):
        full, errors = gate.evaluate(
            [run(4)], legs({4: {"ubuntu-22.04"}}), ON_DEV
        )
        self.assertIsNone(full)
        self.assertEqual(len(errors), 1)

    def test_the_commit_must_be_on_a_release_branch(self):
        off = {"development-denext": False, "rel/3.0.0-rc.7": False}
        full, errors = gate.evaluate([run(5)], legs({5: ALL_LEGS}), off)
        self.assertEqual(full, 5)
        self.assertEqual(len(errors), 1)
        self.assertIn("development-denext, rel/3.0.0-rc.7", errors[0])
        on_rel = {"development-denext": False, "rel/3.0.0-rc.7": True}
        self.assertEqual(
            gate.evaluate([run(5)], legs({5: ALL_LEGS}), on_rel), (5, [])
        )

    def test_release_branches(self):
        self.assertEqual(
            gate.release_branches("3.0.0-rc.7"),
            ["development-denext", "rel/3.0.0-rc.7"],
        )
        self.assertEqual(
            gate.release_branches("3.0.0-denext.1"),
            ["development-denext", "rel/3.0.0-denext.1"],
        )

    def test_only_a_release_candidate_is_a_prerelease(self):
        for version in ("3.0.0-rc.1", "3.0.0-rc.6", "3.0.0-rc.7+ci.3", "3.0.0-rc"):
            self.assertTrue(gate.is_prerelease(version), version)
        # The fork's stable line after its release candidates: full releases.
        for version in (
            "3.0.0-denext.1",
            "3.0.0-denext.2",
            "3.0.0-denext.10",
            "3.0.0-denext.1+ci.4",
            "3.0.0",
            "2.9.7-denext.22",
            "3.0.0-rcx.1",
        ):
            self.assertFalse(gate.is_prerelease(version), version)

    def test_prerelease_flag_cli(self):
        def flag(version):
            return subprocess.run(
                [sys.executable, "-I", GATE, "--prerelease-flag", version],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()

        self.assertEqual(flag("3.0.0-rc.6"), "--prerelease")
        self.assertEqual(flag("3.0.0-denext.1"), "")


class Latest(unittest.TestCase):
    def test_newest_rc(self):
        self.assertFalse(
            gate.is_latest("3.0.0-rc.6", ["3.0.0-rc.5", "2.9.7-denext.22"]),
            "a release candidate is a pre-release, never Latest",
        )
        self.assertGreater(
            gate.version_key("3.0.0-rc.10"), gate.version_key("3.0.0-rc.9")
        )
        self.assertGreater(
            gate.version_key("3.0.0-rc.1"), gate.version_key("2.9.7-denext.22")
        )

    def test_denext_line_is_newer_than_every_rc(self):
        self.assertTrue(
            gate.is_latest("3.0.0-denext.1", ["3.0.0-rc.6", "3.0.0-rc.10"])
        )
        self.assertTrue(
            gate.is_latest("3.0.0-denext.10", ["3.0.0-denext.9", "3.0.0-rc.6"])
        )
        self.assertFalse(gate.is_latest("3.0.0-denext.2", ["3.0.0-denext.10"]))

    def test_release_after_its_prereleases(self):
        self.assertTrue(gate.is_latest("3.0.0", ["3.0.0-denext.12", "3.0.0-rc.6"]))
        self.assertFalse(gate.is_latest("3.0.0-denext.13", ["3.0.0"]))

    def test_the_rule_is_kept_to_the_3_0_0_core(self):
        # elsewhere semver's own order holds: `denext` < `rc`
        self.assertLess(
            gate.version_key("3.0.1-denext.1"), gate.version_key("3.0.1-rc.1")
        )

    def test_an_older_line_is_not_latest(self):
        self.assertFalse(gate.is_latest("2.9.7-denext.23", ["3.0.0-rc.6"]))

    def test_unparseable_others_and_build_metadata(self):
        self.assertTrue(gate.is_latest("3.0.0-denext.1", ["nightly", "3.0.0-rc.5"]))
        self.assertTrue(gate.is_latest("3.0.0-denext.1", ["3.0.0-denext.1+ci.4"]))

    def test_latest_cli(self):
        out = subprocess.run(
            [sys.executable, "-I", GATE, "--latest", "3.0.0-denext.1", "3.0.0-rc.6"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        self.assertEqual(out, "true")


FAKE_GH = r'''#!/usr/bin/env python3
import json, os, sys
answers = json.load(open(os.environ["FAKE_GH_ANSWERS"]))
args = [a for a in sys.argv[1:] if a not in ("api", "--paginate")]
path = args[0]
for prefix, answer in answers.items():
    if path == prefix:
        if answer is None:
            sys.stderr.write("HTTP 404: Not Found\n")
            sys.exit(1)
        sys.stdout.write(answer)
        sys.exit(0)
sys.stderr.write("unexpected gh call: %r\n" % (sys.argv[1:],))
sys.exit(2)
'''

REPO = "Brainwires/deno"
SHA = "abc123"
RUNS = f"repos/{REPO}/actions/workflows/denext_tests.yml/runs?head_sha={SHA}&per_page=100"


def jobs(*names):
    return "".join(f"{n} / test libs\n" for n in names) + "lint / ubuntu-22.04\n"


class Script(unittest.TestCase):
    def gate(self, answers):
        with tempfile.TemporaryDirectory() as tmp:
            gh = os.path.join(tmp, "gh")
            with open(gh, "w") as f:
                f.write(FAKE_GH)
            os.chmod(gh, 0o755)
            answers_path = os.path.join(tmp, "answers.json")
            with open(answers_path, "w") as f:
                json.dump(answers, f)
            env = {
                "PATH": tmp + os.pathsep + os.environ.get("PATH", ""),
                "FAKE_GH_ANSWERS": answers_path,
                "REPO": REPO,
                "SHA": SHA,
                "VERSION": "3.0.0-rc.7",
            }
            return subprocess.run(
                [sys.executable, "-I", GATE],
                env=env,
                capture_output=True,
                text=True,
            )

    def base(self):
        return {
            RUNS: '{"id":11,"status":"completed","conclusion":"success","run_attempt":1}\n',
            f"repos/{REPO}/actions/runs/11/jobs?per_page=100": jobs(*gate.REQUIRED_LEGS),
            f"repos/{REPO}/compare/{SHA}...development-denext": "ahead\n",
            f"repos/{REPO}/compare/{SHA}...rel/3.0.0-rc.7": None,
        }

    def test_passes(self):
        result = self.gate(self.base())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("full_run=11", result.stdout)

    def test_refuses_a_rerun_after_a_failure(self):
        answers = self.base()
        answers[RUNS] = '{"id":11,"status":"completed","conclusion":"success","run_attempt":2}\n'
        answers[f"repos/{REPO}/actions/runs/11/attempts/1"] = "failure\n"
        result = self.gate(answers)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("::error::denext tests failed on this commit: 11", result.stdout)

    def test_refuses_an_unmerged_commit(self):
        answers = self.base()
        answers[f"repos/{REPO}/compare/{SHA}...development-denext"] = "diverged\n"
        result = self.gate(answers)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("::error::the commit is on none of", result.stdout)

    def test_refuses_without_windows_arm64(self):
        answers = self.base()
        legs = [l for l in gate.REQUIRED_LEGS if l != "windows-11-arm"]
        answers[f"repos/{REPO}/actions/runs/11/jobs?per_page=100"] = jobs(*legs)
        result = self.gate(answers)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("lacks: windows-11-arm", result.stdout)


if __name__ == "__main__":
    unittest.main()
