#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests of tag_reuse.py (which builds a tag run takes from an earlier run of
the commit) and of the denext_runtime.yml wiring that uses it.
Run: python3 -I .github/denext-runtime/tag_reuse_test.py"""
import os
import re
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import tag_reuse as tr  # noqa: E402

WORKFLOW = os.path.join(HERE, "..", "workflows", "denext_runtime.yml")
REPO = "Brainwires/deno"
SHA = "a" * 40
LAUFEY = "b" * 40
BLOB = "c" * 40
VERSION = "3.0.0-denext.22"
TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin"]
WANT = {
    "repo": REPO, "sha": SHA, "run_id": "900", "version": VERSION,
    "laufey_sha": LAUFEY, "targets": TARGETS, "blob": BLOB,
}


def run(rid, event="workflow_dispatch", branch="rel/3.0.0-denext.22", **kw):
    r = {
        "id": rid, "event": event, "head_branch": branch, "head_sha": SHA,
        "path": tr.WORKFLOW_PATH, "repository": REPO, "head_repository": REPO,
    }
    r.update(kw)
    return r


def full(version=VERSION, cache="off", laufey=LAUFEY, kinds=tr.ORDER, fail=(), drop=(), expired=(), blob=BLOB):
    """A run's details: every job of `kinds` passed and uploaded, but the
    jobs in `fail`, the artifacts in `drop` (missing) and `expired`."""
    jobs, arts = [], [{"name": tr.plan_marker(version, laufey, cache), "expired": False}]
    for kind in kinds:
        job, prefixes, _ = tr.KINDS[kind]
        for t in TARGETS:
            name = f"{job} / {t}"
            jobs.append({"name": name, "conclusion": "failure" if name in fail else "success"})
            for p in prefixes:
                a = f"{p}-{t}"
                if a not in drop:
                    arts.append({"name": a, "expired": a in expired})
    return {"jobs": jobs, "artifacts": arts, "blob": blob}


def pick(runs, details):
    picks, _ = tr.select(runs, lambda rid: details[rid], WANT)
    return picks


class Marker(unittest.TestCase):
    def test_round_trip(self):
        name = tr.plan_marker("3.0.0-denext.22+ci.17", LAUFEY, "on")
        self.assertEqual(
            tr.parse_marker(name),
            {"version": "3.0.0-denext.22+ci.17", "laufey": LAUFEY, "cache": "on"},
        )

    def test_artifact_name_is_legal(self):
        # upload-artifact refuses " : < > | * ? \\ / and line breaks.
        name = tr.plan_marker(VERSION + "+ci.3", LAUFEY, "off")
        self.assertFalse(set(name) & set('":<>|*?\\/\r\n'))

    def test_garbage(self):
        for name in ("laufey-x86_64-apple-darwin", "plan-v1_version", "plan-v1_version=1_laufey=2"):
            self.assertIsNone(tr.parse_marker(name), name)


class Select(unittest.TestCase):
    def test_a_release_build_dispatch_supplies_everything(self):
        self.assertEqual(pick([run(10)], {10: full()}), {k: 10 for k in tr.ORDER})

    def test_a_ci_build_supplies_nothing(self):
        # rel/** pushes stamp +ci.N into deno --version and the archive names.
        d = {10: full(version=VERSION + "+ci.41", cache="on")}
        self.assertEqual(pick([run(10, event="push")], d), dict.fromkeys(tr.ORDER))

    def test_a_cached_build_supplies_nothing(self):
        self.assertEqual(pick([run(10)], {10: full(cache="on")}), dict.fromkeys(tr.ORDER))

    def test_the_newest_qualifying_run_wins(self):
        d = {10: full(), 11: full()}
        self.assertEqual(pick([run(10), run(11)], d)["cli"], 11)

    def test_never_this_run_another_commit_repo_or_workflow(self):
        bad = [
            run(900),
            run(11, head_sha="d" * 40),
            run(12, head_repository="someone/deno"),
            run(13, repository="someone/deno"),
            run(14, path=".github/workflows/other.yml"),
            run(15, event="push", branch="denext-runtime-v3.0.0-denext.21"),
            run(16, event="pull_request", branch="feature"),
            run(17, event="push", branch="feature/x"),
        ]
        d = {r["id"]: full() for r in bad}
        self.assertEqual(pick(bad, d), dict.fromkeys(tr.ORDER))

    def test_branch_pushes_that_qualify(self):
        for branch in ("rel/3.0.0-denext.22", "development-denext"):
            self.assertEqual(pick([run(10, event="push", branch=branch)], {10: full()})["cli"], 10, branch)

    def test_another_workflow_blob_supplies_nothing(self):
        self.assertEqual(pick([run(10)], {10: full(blob="e" * 40)}), dict.fromkeys(tr.ORDER))

    def test_another_laufey_keeps_denort_bin_and_cli(self):
        p = pick([run(10)], {10: full(laufey="f" * 40)})
        self.assertEqual(p, {"laufey": None, "denort": None, "denort_bin": 10, "cli": 10})

    def test_one_failed_target_rebuilds_that_kind(self):
        p = pick([run(10)], {10: full(fail=("denort / x86_64-apple-darwin",))})
        self.assertEqual(p, {"laufey": 10, "denort": None, "denort_bin": 10, "cli": 10})

    def test_missing_or_expired_artifacts(self):
        p = pick([run(10)], {10: full(drop=("symbols-denort-aarch64-apple-darwin",))})
        self.assertIsNone(p["denort"])
        p = pick([run(10)], {10: full(expired=("laufey-x86_64-apple-darwin",))})
        self.assertIsNone(p["laufey"])
        self.assertEqual(p["denort"], 10)

    def test_cli_never_without_its_denort_bin(self):
        p = pick([run(10)], {10: full(fail=("denort bin / aarch64-apple-darwin",))})
        self.assertIsNone(p["denort_bin"])
        self.assertIsNone(p["cli"])

    def test_cli_only_from_the_run_of_the_denort_bin(self):
        # The newest run has denort bin but a failed cli: the cli builds here
        # on that denort bin; an older run's cli (built on other denort
        # archives) is never taken.
        d = {11: full(fail=("cli / x86_64-apple-darwin",)), 10: full()}
        p = pick([run(10), run(11)], d)
        self.assertEqual(p["denort_bin"], 11)
        self.assertIsNone(p["cli"])

    def test_kinds_from_different_runs(self):
        d = {11: full(kinds=("denort_bin", "cli")), 10: full(kinds=("laufey", "denort"))}
        self.assertEqual(pick([run(10), run(11)], d), {"laufey": 10, "denort": 10, "denort_bin": 11, "cli": 11})

    def test_no_marker(self):
        d = full()
        d["artifacts"] = [a for a in d["artifacts"] if not a["name"].startswith("plan-")]
        self.assertEqual(pick([run(10)], {10: d}), dict.fromkeys(tr.ORDER))

    def test_stops_reading_once_everything_is_found(self):
        asked = []

        def details(rid):
            asked.append(rid)
            return full()

        tr.select([run(10), run(11)], details, WANT)
        self.assertEqual(asked, [11])


class Check(unittest.TestCase):
    ENV = {
        "REPO": REPO, "SHA": SHA, "RUN_ID": "900", "VERSION": VERSION,
        "LAUFEY_SHA": LAUFEY, "TARGETS": ",".join(TARGETS),
    }

    def test_api_error_reuses_nothing(self):
        def boom(*_):
            raise subprocess.CalledProcessError(1, ["gh"], stderr="HTTP 502")

        picks, reasons = tr.check(self.ENV, runs_of=boom, blob_of=lambda *_: BLOB)
        self.assertEqual(picks, dict.fromkeys(tr.ORDER))
        self.assertIn("HTTP 502", reasons[0])
        self.assertTrue(reasons[0].startswith("::warning::"))

    def test_missing_env_reuses_nothing(self):
        picks, _ = tr.check({"REPO": REPO})
        self.assertEqual(picks, dict.fromkeys(tr.ORDER))

    def test_the_blob_is_read_at_the_runs_commit(self):
        seen = []

        def details(repo, rid, head_sha, blobs):
            seen.append(head_sha)
            self.assertEqual(blobs[SHA], BLOB)
            return full()

        picks, _ = tr.check(self.ENV, runs_of=lambda *_: [run(10)], details_of=details, blob_of=lambda *_: BLOB)
        self.assertEqual(picks["cli"], 10)
        self.assertEqual(seen, [SHA])

    def test_main_prints_one_line_per_kind(self):
        out = subprocess.run(
            [sys.executable, "-I", os.path.join(HERE, "tag_reuse.py")],
            env={"PATH": os.environ.get("PATH", "")}, capture_output=True, text=True, check=True,
        ).stdout.splitlines()
        self.assertEqual(out[-4:], [f"reuse_{k}=" for k in tr.ORDER])


def job_block(text, job):
    m = re.search(rf"^  {re.escape(job)}:\n(.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)", text, re.M | re.S)
    if not m:
        raise AssertionError(f"no job {job} in denext_runtime.yml")
    return m.group(1)


class Workflow(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(WORKFLOW, encoding="utf-8") as f:
            cls.text = f.read()

    def test_setup_runs_it_on_tags_with_an_opt_out(self):
        setup = job_block(self.text, "setup")
        self.assertIn(".github/denext-runtime/tag_reuse.py", setup)
        self.assertIn("python3 -I .github/denext-runtime/tag_reuse.py", setup)
        self.assertIn("TAG_REUSE: ${{ vars.DENEXT_RUNTIME_TAG_REUSE }}", setup)
        for kind in tr.ORDER:
            self.assertIn(f"reuse_{kind}: ${{{{ steps.plan.outputs.reuse_{kind} }}}}", setup)

    def test_setup_uploads_the_plan_marker(self):
        setup = job_block(self.text, "setup")
        self.assertIn("name: ${{ steps.plan.outputs.plan_marker }}", setup)
        self.assertIn(tr.PLAN_PREFIX, setup)

    def test_reused_jobs_are_skipped(self):
        for job, kind in (("laufey", "laufey"), ("denort", "denort"), ("denort_bin", "denort_bin"), ("cli", "cli")):
            block = job_block(self.text, job)
            self.assertIn(f"!needs.setup.outputs.reuse_{kind}", block, job)

    def test_downloads_follow_the_reuse(self):
        for job, kind in (("package", "denort"), ("package", "laufey"), ("cli", "denort_bin"), ("e2e_fork", "cli")):
            self.assertIn(f"run-id: ${{{{ needs.setup.outputs.reuse_{kind} || github.run_id }}}}", job_block(self.text, job), (job, kind))
        publish = job_block(self.text, "publish")
        for kind in ("cli", "denort", "denort_bin"):
            self.assertIn(f"needs.setup.outputs.reuse_{kind}", publish, kind)

    def test_publish_attests_what_it_publishes(self):
        publish = job_block(self.text, "publish")
        self.assertIn("actions/attest-build-provenance", publish)
        self.assertIn("attestations: write", publish)

    def test_release_builds_use_no_cache(self):
        for job in ("laufey", "denort", "denort_bin", "cli"):
            block = job_block(self.text, job)
            self.assertNotIn("if: github.ref_type != 'tag'\n", block, job)
            self.assertIn("github.ref_type != 'tag' && !inputs.release_version", block, job)


if __name__ == "__main__":
    unittest.main()
