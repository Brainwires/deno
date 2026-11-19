#!/usr/bin/env python3
"""Tests for guard_verified.py, the verified-SHA guard of the workflows that
run on a push to development-denext: a push of a commit that already passed
the same workflow on its release branch (a full `denext tests` dispatch) or
its tag build (`denext runtime`) skips its jobs and concludes green; anything
else, an API error included, runs in full. The last cases run the script
against a stand-in `gh` that answers from canned API responses.

Usage: python3 -I .github/denext-runtime/guard_verified_test.py
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
GUARD = os.path.join(HERE, "guard_verified.py")

spec = importlib.util.spec_from_file_location("guard_verified", GUARD)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

SHA = "07696147c0"
OWN = 42949767011
PUSH = {"EVENT": "push", "REF": "refs/heads/development-denext"}


def run(id, event, branch, conclusion="success", status="completed", sha=SHA):
    return {
        "id": id,
        "event": event,
        "head_branch": branch,
        "head_sha": sha,
        "status": status,
        "conclusion": conclusion,
    }


def jobs(names, conclusion="success"):
    return [{"name": n, "conclusion": conclusion} for n in names]


def full_tests_jobs():
    return jobs(f"{leg} / test libs" for leg in guard.tag_gate.REQUIRED_LEGS) + jobs(
        ["lint / ubuntu-22.04"]
    )


def full_runtime_jobs():
    return jobs(
        f"{job} / {target}"
        for job in guard.RUNTIME_JOBS
        for target in guard.tag_gate.LAUFEY_TARGETS
    )


def table(by_id):
    return lambda run_id: by_id.get(run_id, [])


def env(**extra):
    return {**PUSH, "REPO": "Brainwires/deno", "SHA": SHA, "RUN_ID": str(OWN), **extra}


class Decide(unittest.TestCase):
    def test_verified_by_a_full_dispatch_on_rel(self):
        # 07696147c0: the push run beside the green rel/3.0.0-denext.18 dispatch.
        runs = [
            run(OWN, "push", "development-denext", None, "in_progress"),
            run(42889654613, "workflow_dispatch", "rel/3.0.0-denext.18"),
        ]
        ok, reason = guard.decide(
            guard.TESTS, SHA, OWN, runs, table({42889654613: full_tests_jobs()})
        )
        self.assertTrue(ok, reason)
        self.assertIn("42889654613", reason)

    def test_verified_by_the_tag_build(self):
        runs = [
            run(42902232675, "push", "denext-runtime-v3.0.0-denext.18"),
            run(OWN, "push", "development-denext", None, "in_progress"),
        ]
        ok, reason = guard.decide(guard.RUNTIME, SHA, OWN, runs, table({}))
        self.assertTrue(ok, reason)
        self.assertIn("42902232675", reason)

    def test_runtime_verified_by_a_full_rel_dispatch(self):
        runs = [run(5, "workflow_dispatch", "rel/3.0.0-denext.18")]
        ok, _ = guard.decide(
            guard.RUNTIME, SHA, OWN, runs, table({5: full_runtime_jobs()})
        )
        self.assertTrue(ok)

    def test_not_verified_without_another_run(self):
        runs = [run(OWN, "push", "development-denext", None, "in_progress")]
        ok, reason = guard.decide(guard.TESTS, SHA, OWN, runs, table({}))
        self.assertFalse(ok)
        self.assertIn("running in full", reason)

    def test_a_linux_only_dispatch_does_not_verify(self):
        # full=false: the other legs are skipped, so it never vouches.
        runs = [run(7, "workflow_dispatch", "rel/3.0.0-denext.18")]
        ok, _ = guard.decide(
            guard.TESTS,
            SHA,
            OWN,
            runs,
            table({7: jobs(["ubuntu-22.04 / test libs", "lint / ubuntu-22.04"])}),
        )
        self.assertFalse(ok)

    def test_a_partial_runtime_dispatch_does_not_verify(self):
        # cli_only / a targets filter: the laufey and package jobs are missing.
        runs = [run(8, "workflow_dispatch", "rel/3.0.0-denext.18")]
        partial = [j for j in full_runtime_jobs() if not j["name"].startswith("laufey")]
        ok, _ = guard.decide(guard.RUNTIME, SHA, OWN, runs, table({8: partial}))
        self.assertFalse(ok)

    def test_verified_by_a_full_dispatch_on_a_train_branch(self):
        # 3.0.0-denext.22 ran on train/denext22: its development-denext push
        # re-ran everything in full and held the tag for 1h45m.
        runs = [
            run(OWN, "push", "development-denext", None, "in_progress"),
            run(43519114893, "workflow_dispatch", "train/denext22"),
        ]
        ok, reason = guard.decide(
            guard.TESTS, SHA, OWN, runs, table({43519114893: full_tests_jobs()})
        )
        self.assertTrue(ok, reason)
        self.assertIn("43519114893", reason)

    def test_runtime_verified_by_a_full_train_dispatch(self):
        runs = [run(6, "workflow_dispatch", "train/denext22")]
        ok, _ = guard.decide(
            guard.RUNTIME, SHA, OWN, runs, table({6: full_runtime_jobs()})
        )
        self.assertTrue(ok)

    def test_npm_matrix_verified_by_a_train_push(self):
        runs = [run(12, "push", "train/denext22")]
        ok, _ = guard.decide(guard.NPM_MATRIX, SHA, OWN, runs, table({}))
        self.assertTrue(ok)

    def test_a_partial_train_dispatch_does_not_verify(self):
        runs = [run(7, "workflow_dispatch", "train/denext22")]
        ok, _ = guard.decide(
            guard.TESTS,
            SHA,
            OWN,
            runs,
            table({7: jobs(["ubuntu-22.04 / test libs", "lint / ubuntu-22.04"])}),
        )
        self.assertFalse(ok)

    def test_a_branch_merely_named_like_a_train_does_not_verify(self):
        for branch in ("trains/x", "fix/train/x", "feat/train-x"):
            runs = [run(9, "workflow_dispatch", branch)]
            ok, _ = guard.decide(
                guard.TESTS, SHA, OWN, runs, table({9: full_tests_jobs()})
            )
            self.assertFalse(ok, branch)

    def test_a_dispatch_from_a_fix_branch_does_not_verify(self):
        runs = [run(9, "workflow_dispatch", "fix/something")]
        ok, _ = guard.decide(guard.TESTS, SHA, OWN, runs, table({9: full_tests_jobs()}))
        self.assertFalse(ok)

    def test_an_earlier_development_denext_push_does_not_verify(self):
        # Only another event or ref vouches, never the guarded ref itself.
        runs = [run(10, "push", "development-denext")]
        ok, _ = guard.decide(guard.NPM_MATRIX, SHA, OWN, runs, table({}))
        self.assertFalse(ok)

    def test_npm_matrix_verified_by_a_rel_push(self):
        runs = [run(11, "push", "rel/3.0.0-denext.18")]
        ok, _ = guard.decide(guard.NPM_MATRIX, SHA, OWN, runs, table({}))
        self.assertTrue(ok)

    def test_a_failed_or_running_dispatch_does_not_verify(self):
        for conclusion, status in (("cancelled", "completed"), (None, "in_progress")):
            runs = [run(12, "workflow_dispatch", "rel/x", conclusion, status)]
            ok, _ = guard.decide(
                guard.TESTS, SHA, OWN, runs, table({12: full_tests_jobs()})
            )
            self.assertFalse(ok, (conclusion, status))

    def test_the_tag_build_under_way_stands_for_the_push(self):
        # 512a5572e7: the branch pushed with the tag built the commit a
        # second time beside the tag build.
        for status in ("queued", "in_progress"):
            runs = [
                run(44469579126, "push", "denext-runtime-v3.0.0-denext.27", None, status),
                run(OWN, "push", "development-denext", None, "in_progress"),
            ]
            ok, reason = guard.decide(guard.RUNTIME, SHA, OWN, runs, table({}))
            self.assertTrue(ok, reason)
            self.assertIn("44469579126", reason)
            self.assertIn("under way", reason)

    def test_a_tag_build_under_way_never_stands_for_the_tests(self):
        runs = [run(9, "push", "denext-runtime-v3.0.0-denext.27", None, "in_progress")]
        ok, _ = guard.decide(guard.TESTS, SHA, OWN, runs, table({}))
        self.assertFalse(ok)

    def test_a_failed_tag_build_forces_a_full_run(self):
        # The tag gate refused the first attempt: the push builds in full.
        runs = [run(9, "push", "denext-runtime-v3.0.0-denext.27", "failure")]
        ok, reason = guard.decide(guard.RUNTIME, SHA, OWN, runs, table({}))
        self.assertFalse(ok)
        self.assertIn("failed", reason)

    def test_a_train_dispatch_under_way_is_not_a_verdict_but_is_awaited(self):
        runs = [
            run(5, "workflow_dispatch", "train/denext27", None, "in_progress"),
            run(OWN, "push", "development-denext", None, "in_progress"),
        ]
        for workflow in (guard.RUNTIME, guard.TESTS):
            ok, _ = guard.decide(workflow, SHA, OWN, runs, table({}))
            self.assertFalse(ok)
            self.assertEqual(guard.awaited(workflow, SHA, OWN, runs), [5])

    def test_nothing_is_awaited_from_other_kinds_or_beside_a_red_run(self):
        going = lambda event, branch: run(5, event, branch, None, "in_progress")
        for workflow, r in (
            (guard.TESTS, going("push", "train/denext27")),
            (guard.TESTS, going("workflow_dispatch", "fix/x")),
            (guard.RUNTIME, going("push", "development-denext")),
            (guard.NPM_MATRIX, going("workflow_dispatch", "train/denext27")),
            (guard.RUNTIME, run(5, "workflow_dispatch", "train/denext27")),
        ):
            self.assertEqual(guard.awaited(workflow, SHA, OWN, [r]), [], r)
        runs = [
            going("workflow_dispatch", "train/denext27"),
            run(6, "push", "development-denext", "failure"),
        ]
        self.assertEqual(guard.awaited(guard.RUNTIME, SHA, OWN, runs), [])
        # This run itself is never awaited.
        own = run(OWN, "workflow_dispatch", "train/denext27", None, "in_progress")
        self.assertEqual(guard.awaited(guard.RUNTIME, SHA, OWN, [own]), [])

    def test_a_red_run_of_the_commit_forces_a_full_run(self):
        runs = [
            run(13, "workflow_dispatch", "rel/x"),
            run(14, "push", "rel/x", "failure"),
        ]
        ok, reason = guard.decide(
            guard.TESTS, SHA, OWN, runs, table({13: full_tests_jobs()})
        )
        self.assertFalse(ok)
        self.assertIn("14", reason)

    def test_another_commit_never_verifies(self):
        runs = [run(15, "push", "denext-runtime-v3.0.0-denext.18", sha="other")]
        ok, _ = guard.decide(guard.RUNTIME, SHA, OWN, runs, table({}))
        self.assertFalse(ok)


class Check(unittest.TestCase):
    def test_only_a_development_denext_push_is_guarded(self):
        called = []

        def runs_of(*args):
            called.append(args)
            return [run(1, "push", "denext-runtime-v1")]

        for e in (
            {"EVENT": "workflow_dispatch", "REF": "refs/heads/development-denext"},
            {"EVENT": "push", "REF": "refs/heads/rel/3.0.0-denext.18"},
            {"EVENT": "pull_request", "REF": "refs/pull/1/merge"},
        ):
            ok, _ = guard.check(guard.RUNTIME, env(**e), runs_of, lambda *a: [])
            self.assertFalse(ok, e)
        self.assertEqual(called, [])

    def waiting(self, reads, wait, workflow=None, by_id=None):
        """check() over successive reads of the runs; returns (verified,
        reason, the sleeps it took)."""
        reads = list(reads)
        now = [0.0]
        sleeps = []

        def runs_of(repo, wf, sha):
            return reads.pop(0) if len(reads) > 1 else reads[0]

        def sleep(seconds):
            sleeps.append(seconds)
            now[0] += seconds

        ok, reason = guard.check(
            workflow or guard.RUNTIME,
            env(GUARD_WAIT=str(wait)),
            runs_of=runs_of,
            jobs_of=lambda repo, run_id: (by_id or {}).get(run_id, []),
            sleep=sleep,
            clock=lambda: now[0],
        )
        return ok, reason, sleeps

    def test_waits_for_the_train_run_that_ends_green(self):
        # 44452735377 ended 4 min 30 s after the branch was pushed.
        going = [run(5, "workflow_dispatch", "train/denext27", None, "in_progress")]
        done = [run(5, "workflow_dispatch", "train/denext27")]
        ok, reason, sleeps = self.waiting(
            [going, going, done], 1200, by_id={5: full_runtime_jobs()}
        )
        self.assertTrue(ok, reason)
        self.assertEqual(sleeps, [30, 30])

    def test_the_wait_ends_at_its_limit_with_a_full_run(self):
        going = [run(5, "workflow_dispatch", "train/denext27", None, "in_progress")]
        ok, reason, sleeps = self.waiting([going], 70)
        self.assertFalse(ok)
        self.assertEqual(sleeps, [30, 30, 10])
        self.assertIn("running in full", reason)

    def test_a_run_that_ends_red_ends_the_wait_with_a_full_run(self):
        going = [run(5, "workflow_dispatch", "train/denext27", None, "in_progress")]
        red = [run(5, "workflow_dispatch", "train/denext27", "failure")]
        ok, reason, sleeps = self.waiting([going, red], 1200)
        self.assertFalse(ok)
        self.assertEqual(sleeps, [30])
        self.assertIn("failed", reason)

    def test_no_wait_without_a_limit_or_a_run_to_wait_for(self):
        going = [run(5, "workflow_dispatch", "train/denext27", None, "in_progress")]
        self.assertEqual(self.waiting([going], 0)[2], [])
        other = [run(5, "push", "development-denext", None, "in_progress")]
        self.assertEqual(self.waiting([other], 1200)[2], [])
        self.assertEqual(guard.guard_wait("soon"), 0)
        self.assertEqual(guard.guard_wait(None), 0)

    def test_an_api_error_runs_in_full(self):
        def broken(*args):
            raise subprocess.CalledProcessError(1, ["gh"], stderr="HTTP 502: Bad Gateway")

        ok, reason = guard.check(guard.TESTS, env(), broken, lambda *a: [])
        self.assertFalse(ok)
        self.assertIn("::warning::", reason)
        self.assertIn("HTTP 502", reason)

    def test_an_api_error_reading_jobs_runs_in_full(self):
        def runs_of(*args):
            return [run(1, "workflow_dispatch", "rel/x")]

        def broken_jobs(*args):
            raise ValueError("Expecting value: line 1 column 1 (char 0)")

        ok, reason = guard.check(guard.TESTS, env(), runs_of, broken_jobs)
        self.assertFalse(ok)
        self.assertIn("running in full", reason)


FAKE_GH = r'''#!/usr/bin/env python3
import json, os, sys
answers = json.load(open(os.environ["FAKE_GH_ANSWERS"]))
args = [a for a in sys.argv[1:] if a not in ("api", "--paginate")]
path = args[0]
for prefix, answer in answers.items():
    if path == prefix:
        if answer is None:
            sys.stderr.write("HTTP 500: Internal Server Error\n")
            sys.exit(1)
        sys.stdout.write(answer)
        sys.exit(0)
sys.stderr.write("unexpected gh call: %r\n" % (sys.argv[1:],))
sys.exit(2)
'''

REPO = "Brainwires/deno"


def runs_path(workflow):
    return f"repos/{REPO}/actions/workflows/{workflow}/runs?head_sha={SHA}&per_page=100"


class Script(unittest.TestCase):
    def guard(self, workflow, answers, **extra):
        with tempfile.TemporaryDirectory() as tmp:
            gh = os.path.join(tmp, "gh")
            with open(gh, "w") as f:
                f.write(FAKE_GH)
            os.chmod(gh, 0o755)
            answers_path = os.path.join(tmp, "answers.json")
            with open(answers_path, "w") as f:
                json.dump(answers, f)
            e = {
                "PATH": tmp + os.pathsep + os.environ.get("PATH", ""),
                "FAKE_GH_ANSWERS": answers_path,
                **env(REPO=REPO, **extra),
            }
            return subprocess.run(
                [sys.executable, "-I", GUARD, workflow],
                env=e,
                capture_output=True,
                text=True,
            )

    def line(self, r):
        return json.dumps(r) + "\n"

    def test_verified_by_a_dispatch(self):
        answers = {
            runs_path(guard.TESTS): self.line(run(21, "workflow_dispatch", "rel/3.0.0-denext.18"))
            + self.line(run(OWN, "push", "development-denext", None, "in_progress")),
            f"repos/{REPO}/actions/runs/21/jobs?per_page=100": "".join(
                self.line(j) for j in full_tests_jobs()
            ),
        }
        p = self.guard(guard.TESTS, answers)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.splitlines()[-1], "verified=true", p.stdout)

    def test_verified_by_a_tag_build(self):
        answers = {
            runs_path(guard.RUNTIME): self.line(
                run(22, "push", "denext-runtime-v3.0.0-denext.18")
            ),
        }
        p = self.guard(guard.RUNTIME, answers)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.splitlines()[-1], "verified=true", p.stdout)

    def test_not_verified(self):
        answers = {
            runs_path(guard.TESTS): self.line(
                run(OWN, "push", "development-denext", None, "in_progress")
            ),
        }
        p = self.guard(guard.TESTS, answers)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.splitlines()[-1], "verified=false", p.stdout)

    def test_an_api_error_falls_through_to_a_full_run(self):
        p = self.guard(guard.RUNTIME, {runs_path(guard.RUNTIME): None})
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.splitlines()[-1], "verified=false", p.stdout)
        self.assertIn("::warning::", p.stdout)
        self.assertIn("HTTP 500", p.stdout)

    def test_unparseable_api_output_falls_through_to_a_full_run(self):
        p = self.guard(guard.TESTS, {runs_path(guard.TESTS): "<html>oops</html>\n"})
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.splitlines()[-1], "verified=false", p.stdout)

    def test_a_bad_workflow_name_is_a_usage_error(self):
        p = self.guard("other.yml", {})
        self.assertEqual(p.returncode, 2)


if __name__ == "__main__":
    unittest.main()
