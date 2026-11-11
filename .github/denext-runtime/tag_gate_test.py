#!/usr/bin/env python3
"""Tests for tag_gate.py, denext_runtime.yml's release gate: a commit is
released only with a full green `denext tests` run (every OS leg, Windows
arm64 included), no failed run or attempt, nothing still running, and only
from development-denext or its release branch, and only when an earlier
`denext runtime` run of the commit built the pinned laufey's native hosts
green on every target, the pin matching cli/rt_desktop/Cargo.toml. The last
cases run the script against a stand-in `gh` that answers from canned API
responses.

Usage: python3 -I .github/denext-runtime/tag_gate_test.py
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timezone

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

    def test_a_run_that_lost_its_jobs_to_the_runner_pool_is_not_red(self):
        # GitHub cancels jobs no runner picked up and concludes the run
        # `failure`: no test failed, so it doesn't block a later green run.
        full, errors = gate.evaluate(
            [run(4, conclusion=gate.NO_RUNNER), run(5)],
            legs({5: ALL_LEGS}),
            ON_DEV,
        )
        self.assertEqual((full, errors), (5, []))

    def test_classify(self):
        self.assertEqual(
            gate.classify("failure", ["success", "cancelled", "skipped"]),
            gate.NO_RUNNER,
        )
        self.assertEqual(
            gate.classify("failure", ["success", "failure", "cancelled"]),
            "failure",
        )
        self.assertEqual(gate.classify("failure", ["timed_out"]), "failure")
        # No jobs at all (a workflow that never started): keep it red.
        self.assertEqual(gate.classify("failure", []), "failure")
        self.assertEqual(gate.classify("success", []), "success")
        self.assertEqual(
            gate.classify("startup_failure", ["cancelled"]), "startup_failure"
        )

    def test_a_run_in_progress_holds_the_release(self):
        _, errors = gate.evaluate(
            [run(1), run(2, None, "in_progress")],
            legs({1: ALL_LEGS}),
            ON_DEV,
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("in progress", errors[0])
        self.assertIn("2", errors[0])

    def test_a_run_stuck_in_the_queue_does_not_hold_the_release(self):
        # 41873491464: queued with no jobs for over a day; cancel and delete
        # both failed, and it held 3.0.0-denext.11.
        stuck = dict(run(41873491464, None, "queued"), stuck=True)
        full, errors = gate.evaluate(
            [run(1), stuck], legs({1: ALL_LEGS}), ON_DEV
        )
        self.assertEqual(errors, [])
        self.assertEqual(full, 1)

    def test_a_stuck_run_vouches_for_nothing(self):
        stuck = dict(run(5, None, "queued"), stuck=True)
        full, errors = gate.evaluate([stuck], legs({5: ALL_LEGS}), ON_DEV)
        self.assertIsNone(full)
        self.assertEqual(len(errors), 1)
        self.assertIn("no successful full", errors[0])

    def test_is_stuck(self):
        now = datetime(2026, 11, 5, 18, 0, tzinfo=timezone.utc)
        old, fresh = "2026-11-04T17:02:01Z", "2026-11-05T17:30:00Z"
        self.assertTrue(gate.is_stuck("queued", 0, old, now))
        self.assertFalse(gate.is_stuck("queued", 0, fresh, now))
        self.assertFalse(gate.is_stuck("queued", 3, old, now))
        self.assertFalse(gate.is_stuck("in_progress", 0, old, now))
        self.assertFalse(gate.is_stuck("queued", 0, None, now))

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


LAUFEY = "50a2fcaf00d3656de1d88215dca5cf7f698979a1"
OTHER_LAUFEY = "3dfcea49fc0000000000000000000000000000aa"
MANIFEST = (
    "[dependencies]\n"
    f'laufey = {{ git = "https://github.com/Brainwires/laufey", rev = "{LAUFEY}" }}\n'
)


def laufey_job(target, conclusion="success", sha=LAUFEY):
    return {
        "name": f"laufey / {target}",
        "conclusion": conclusion,
        "steps": ["Set up job", f"{gate.LAUFEY_STEP} {sha}", "Build webview host"],
    }


def laufey_jobs(**overrides):
    """A run's laufey jobs, every target green from LAUFEY unless overridden
    (target -> job dict, or None to leave the job out)."""
    out = []
    for t in gate.LAUFEY_TARGETS:
        job = overrides.get(t.replace("-", "_"), laufey_job(t))
        if job is not None:
            out.append(job)
    return out


class Laufey(unittest.TestCase):
    def evaluate(self, table):
        return gate.evaluate_laufey(
            list(table), lambda run_id: table[run_id], LAUFEY, "o/r", "abc",
            ["development-denext", "rel/3.0.0-denext.21"],
        )

    def test_targets_match_the_release_matrix(self):
        spec = importlib.util.spec_from_file_location(
            "plan_matrix", os.path.join(HERE, "plan_matrix.py")
        )
        plan = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(plan)
        self.assertEqual(
            sorted(gate.LAUFEY_TARGETS),
            sorted(t["target"] for t in plan.ALL_TARGETS),
        )

    def test_a_run_that_built_every_target_vouches(self):
        run_id, errors = self.evaluate({5: laufey_jobs()})
        self.assertEqual((run_id, errors), (5, []))

    def test_the_denext_19_failure_is_refused(self):
        # Every `denext tests` leg was green, but laufey's Windows x64 host
        # did not compile (MSVC C2589): no run vouches.
        run_id, errors = self.evaluate(
            {5: laufey_jobs(x86_64_pc_windows_msvc=laufey_job(
                "x86_64-pc-windows-msvc", "failure"))}
        )
        self.assertIsNone(run_id)
        self.assertEqual(len(errors), 1)
        self.assertIn("gh workflow run denext_runtime.yml -R o/r", errors[0])
        self.assertIn("-f targets=all", errors[0])
        self.assertIn("development-denext or rel/3.0.0-denext.21", errors[0])

    def test_no_runtime_run_at_all_is_refused(self):
        run_id, errors = self.evaluate({})
        self.assertIsNone(run_id)
        self.assertIn("built the pinned laufey", errors[0])

    def test_a_partial_dispatch_does_not_vouch(self):
        # A dispatch with targets=x86_64-pc-windows-msvc only (the .20 fix).
        only = {t.replace("-", "_"): None for t in gate.LAUFEY_TARGETS
                if t != "x86_64-pc-windows-msvc"}
        run_id, errors = self.evaluate({5: laufey_jobs(**only)})
        self.assertIsNone(run_id)
        self.assertEqual(len(errors), 1)

    def test_a_build_of_another_laufey_does_not_vouch(self):
        # A dispatch with laufey_ref set to another commit.
        other = {t.replace("-", "_"): laufey_job(t, sha=OTHER_LAUFEY)
                 for t in gate.LAUFEY_TARGETS}
        run_id, _ = self.evaluate({5: laufey_jobs(**other)})
        self.assertIsNone(run_id)

    def test_one_full_run_among_partial_ones_vouches(self):
        only = {t.replace("-", "_"): None for t in gate.LAUFEY_TARGETS[1:]}
        run_id, errors = self.evaluate({4: laufey_jobs(**only), 9: laufey_jobs()})
        self.assertEqual((run_id, errors), (9, []))

    def test_pin_matches_the_crate(self):
        self.assertEqual(gate.pinned_rev(MANIFEST), LAUFEY)
        self.assertEqual(gate.check_pin(LAUFEY, MANIFEST), [])

    def test_pin_that_differs_from_the_crate_is_refused(self):
        errors = gate.check_pin(OTHER_LAUFEY, MANIFEST)
        self.assertEqual(len(errors), 1)
        self.assertIn("move both to the same commit", errors[0])

    def test_a_missing_or_short_pin_is_refused(self):
        self.assertEqual(len(gate.check_pin("", MANIFEST)), 1)
        self.assertEqual(len(gate.check_pin("50a2fca", MANIFEST)), 1)
        self.assertEqual(len(gate.check_pin(LAUFEY, "[dependencies]\n")), 1)

    def test_the_real_manifest_names_a_pin(self):
        root = os.path.dirname(os.path.dirname(HERE))
        with open(os.path.join(root, gate.LAUFEY_MANIFEST)) as f:
            self.assertIsNotNone(gate.pinned_rev(f.read()))

    def test_the_workflow_pin_matches_the_crate(self):
        root = os.path.dirname(os.path.dirname(HERE))
        with open(os.path.join(root, ".github/workflows/denext_runtime.yml")) as f:
            workflow = f.read()
        with open(os.path.join(root, gate.LAUFEY_MANIFEST)) as f:
            manifest = f.read()
        import re
        pin = re.search(r"^  LAUFEY_SHA: ([0-9a-f]{40})$", workflow, re.MULTILINE)
        self.assertIsNotNone(pin, "no LAUFEY_SHA in denext_runtime.yml")
        self.assertEqual(gate.check_pin(pin.group(1), manifest), [])
        # The laufey checkout step's name is what the gate reads.
        self.assertIn(
            f"- name: {gate.LAUFEY_STEP} ${{{{ needs.setup.outputs.laufey_sha }}}}",
            workflow,
        )


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
RUNTIME_RUNS = f"repos/{REPO}/actions/workflows/denext_runtime.yml/runs?head_sha={SHA}&per_page=100"
OWN_RUN = "99"


def jobs(*names):
    return "".join(f"{n} / test libs\n" for n in names) + "lint / ubuntu-22.04\n"


class Script(unittest.TestCase):
    def gate(self, answers, manifest=MANIFEST, laufey=LAUFEY):
        with tempfile.TemporaryDirectory() as tmp:
            manifest_path = os.path.join(tmp, "Cargo.toml")
            with open(manifest_path, "w") as f:
                f.write(manifest)
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
                "LAUFEY_SHA": laufey,
                "LAUFEY_MANIFEST": manifest_path,
                "RUN_ID": OWN_RUN,
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
            # The tag build's own run (left out) and a green push run.
            RUNTIME_RUNS: f"{OWN_RUN}\n21\n",
            f"repos/{REPO}/actions/runs/21/jobs?per_page=100": "".join(
                json.dumps(j) + "\n" for j in laufey_jobs()
            ),
        }

    def test_passes(self):
        result = self.gate(self.base())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("full_run=11", result.stdout)

    def test_refuses_a_rerun_after_a_failure(self):
        answers = self.base()
        answers[RUNS] = '{"id":11,"status":"completed","conclusion":"success","run_attempt":2}\n'
        answers[f"repos/{REPO}/actions/runs/11/attempts/1"] = "failure\n"
        answers[f"repos/{REPO}/actions/runs/11/attempts/1/jobs?per_page=100"] = (
            "success\nfailure\ncancelled\n"
        )
        result = self.gate(answers)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("::error::denext tests failed on this commit: 11", result.stdout)

    def test_an_attempt_that_only_lost_runners_does_not_block(self):
        answers = self.base()
        answers[RUNS] = (
            '{"id":10,"status":"completed","conclusion":"failure","run_attempt":1}\n'
            '{"id":11,"status":"completed","conclusion":"success","run_attempt":1}\n'
        )
        answers[f"repos/{REPO}/actions/runs/10/attempts/1/jobs?per_page=100"] = (
            "success\ncancelled\ncancelled\nskipped\n"
        )
        answers[f"repos/{REPO}/actions/runs/10/jobs?per_page=100"] = jobs("ubuntu-22.04")
        result = self.gate(answers)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("full_run=11", result.stdout)

    def test_a_run_stuck_in_the_queue_does_not_block(self):
        answers = self.base()
        answers[RUNS] = (
            '{"id":10,"status":"queued","conclusion":null,"run_attempt":1,'
            '"created_at":"2020-01-01T00:00:00Z"}\n'
            '{"id":11,"status":"completed","conclusion":"success","run_attempt":1}\n'
        )
        answers[f"repos/{REPO}/actions/runs/10/jobs?per_page=1"] = "0\n"
        result = self.gate(answers)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("run 10 is stuck in the queue", result.stdout)

    def test_a_queued_run_with_jobs_still_blocks(self):
        answers = self.base()
        answers[RUNS] = (
            '{"id":10,"status":"queued","conclusion":null,"run_attempt":1,'
            '"created_at":"2020-01-01T00:00:00Z"}\n'
            '{"id":11,"status":"completed","conclusion":"success","run_attempt":1}\n'
        )
        answers[f"repos/{REPO}/actions/runs/10/jobs?per_page=1"] = "6\n"
        result = self.gate(answers)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("still in progress for this commit: 10", result.stdout)

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


    def test_passes_names_the_laufey_run(self):
        result = self.gate(self.base())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("laufey_run=21", result.stdout)

    def test_refuses_when_laufey_failed_on_windows(self):
        # 3.0.0-denext.19: the full `denext tests` run was green, the pinned
        # laufey's Windows x64 host did not compile.
        answers = self.base()
        answers[f"repos/{REPO}/actions/runs/21/jobs?per_page=100"] = "".join(
            json.dumps(j) + "\n"
            for j in laufey_jobs(x86_64_pc_windows_msvc=laufey_job(
                "x86_64-pc-windows-msvc", "failure"))
        )
        result = self.gate(answers)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("green for: x86_64-pc-windows-msvc", result.stdout)
        self.assertIn(
            f"::error::no denext runtime run of this commit built the pinned laufey ({LAUFEY})",
            result.stdout,
        )
        self.assertIn(f"gh workflow run denext_runtime.yml -R {REPO} --ref <branch> -f targets=all", result.stdout)

    def test_the_tag_builds_own_run_never_vouches(self):
        answers = self.base()
        answers[RUNTIME_RUNS] = f"{OWN_RUN}\n"
        answers[f"repos/{REPO}/actions/runs/{OWN_RUN}/jobs?per_page=100"] = "".join(
            json.dumps(j) + "\n" for j in laufey_jobs()
        )
        result = self.gate(answers)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("built the pinned laufey", result.stdout)

    def test_refuses_a_pin_that_differs_from_the_crate(self):
        result = self.gate(self.base(), laufey=OTHER_LAUFEY)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("move both to the same commit", result.stdout)


if __name__ == "__main__":
    unittest.main()
