#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests for pick_runner.py: our pool while it has a free slot, GitHub's image
for the overflow, the image on a fork's pull request, and the pool whenever
the API fails.

Usage: python3 -I .github/denext-runtime/pick_runner_test.py
"""

import importlib.util
import json
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location(
    "pick_runner", os.path.join(HERE, "pick_runner.py")
)
pick = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pick)

POOL = '["self-hosted","denext-windows-x64"]'
LABEL = "denext-windows-x64"
KEYS = "node_compat_0 node_compat_1 unit_node"


def env(**kw):
    base = {
        "PICK_KEYS": KEYS,
        "PICK_RUNNER": POOL,
        "PICK_IMAGE": "windows-2022",
        "PICK_SLOTS": "1",
        "PICK_OWN_SKIP": " / test node_compat| / test unit_node",
        "GITHUB_REPOSITORY": "o/r",
        "GITHUB_RUN_ID": "7",
    }
    base.update(kw)
    return base


def job(status, name="x / test libs", labels=("self-hosted", LABEL)):
    return {"status": status, "name": name, "labels": list(labels)}


def fake(runs):
    """A GET over {run id: [jobs]} (every run listed as in progress)."""

    def get(path):
        if "/actions/runs?status=queued" in path:
            return {"workflow_runs": []}
        if "/actions/runs?status=in_progress" in path:
            return {"workflow_runs": [{"id": i} for i in runs]}
        run_id = int(path.split("/actions/runs/")[1].split("/")[0])
        return {"jobs": runs[run_id]}

    return get


class Pick(unittest.TestCase):
    def test_parse_runner(self):
        self.assertEqual(pick.parse_runner(POOL), (POOL, LABEL))
        self.assertEqual(
            pick.parse_runner("denext-macos-x64"),
            ('"denext-macos-x64"', "denext-macos-x64"),
        )
        self.assertEqual(pick.parse_runner(""), (None, None))
        self.assertEqual(pick.parse_runner('["self-hosted"]'), (None, None))
        self.assertEqual(pick.parse_runner("[not json"), (None, None))

    def test_an_idle_pool_takes_its_slots_and_github_the_rest(self):
        picks = pick.pick(env(), fake({}))
        self.assertEqual(picks["node_compat_0"], POOL)
        self.assertEqual(picks["node_compat_1"], '"windows-2022"')
        self.assertEqual(picks["unit_node"], '"windows-2022"')

    def test_two_slots(self):
        picks = pick.pick(env(PICK_SLOTS="2"), fake({}))
        self.assertEqual(
            [picks[k] for k in KEYS.split()], [POOL, POOL, '"windows-2022"']
        )

    def test_a_busy_pool_overflows_everything(self):
        # denext's windows-tests holds the one slot (another run, same label).
        picks = pick.pick(env(), fake({3: [job("in_progress")]}))
        self.assertEqual(set(picks.values()), {'"windows-2022"'})

    def test_a_queued_job_holds_a_slot(self):
        picks = pick.pick(env(PICK_SLOTS="2"), fake({3: [job("queued")]}))
        self.assertEqual(picks["node_compat_0"], POOL)
        self.assertEqual(picks["node_compat_1"], '"windows-2022"')

    def test_jobs_of_other_pools_and_finished_jobs_hold_nothing(self):
        picks = pick.pick(
            env(),
            fake({3: [job("in_progress", labels=("windows-2022",)),
                      job("completed")]}),
        )
        self.assertEqual(picks["node_compat_0"], POOL)

    def test_this_runs_own_jobs_that_wait_on_the_pick_hold_nothing(self):
        own = [
            job("queued", "windows-2022 / test node_compat (1/2)"),
            job("queued", "windows-2022 / test unit_node"),
        ]
        picks = pick.pick(env(), fake({7: own}))
        self.assertEqual(picks["node_compat_0"], POOL)

    def test_this_runs_other_jobs_on_the_pool_hold_slots(self):
        # The leg's own libs job on the pool (macOS: the whole leg runs there).
        picks = pick.pick(env(), fake({7: [job("in_progress", "macos-15 / test libs")]}))
        self.assertEqual(picks["node_compat_0"], '"windows-2022"')

    def test_shared_labels_take_the_same_slots(self):
        # biscuits: a linux-x64 (24.04) job takes one of the slots the 22.04
        # pool shares with it.
        e = env(
            PICK_RUNNER='["self-hosted","denext-linux-x64-2204"]',
            PICK_IMAGE="ubuntu-22.04",
            PICK_SLOTS="2",
            PICK_SHARED_LABELS="denext-linux-x64 denext-linux-x64-docker",
        )
        busy = {3: [job("in_progress", labels=("self-hosted", "denext-linux-x64"))]}
        picks = pick.pick(e, fake(busy))
        self.assertEqual(
            [picks[k] for k in KEYS.split()],
            ['["self-hosted","denext-linux-x64-2204"]', '"ubuntu-22.04"', '"ubuntu-22.04"'],
        )
        # Without the shared labels that job would not count.
        picks = pick.pick({**e, "PICK_SHARED_LABELS": ""}, fake(busy))
        self.assertEqual(picks["node_compat_1"], '["self-hosted","denext-linux-x64-2204"]')

    def test_a_busy_runner_of_another_repository_counts(self):
        runners = {
            "runners": [
                {"status": "online", "busy": True, "labels": [{"name": LABEL}]},
            ]
        }
        picks = pick.pick(env(), fake({}), lambda path: runners)
        self.assertEqual(picks["node_compat_0"], '"windows-2022"')

    def test_a_fork_pull_request_never_takes_the_pool(self):
        def no_api(path):
            raise AssertionError("no API call on a fork's pull request")

        picks = pick.pick(env(PICK_FORK_PR="true"), no_api)
        self.assertEqual(set(picks.values()), {'"windows-2022"'})

    def test_no_pool_variable_means_the_image(self):
        picks = pick.pick(env(PICK_RUNNER=""), None)
        self.assertEqual(set(picks.values()), {'"windows-2022"'})

    def test_an_api_failure_keeps_the_pool(self):
        def broken(path):
            raise OSError("timed out")

        warnings = []
        picks = pick.pick(env(), broken, warn=warnings.append)
        self.assertEqual(set(picks.values()), {POOL})
        self.assertIn("timed out", warnings[0])

    def test_a_malformed_answer_keeps_the_pool(self):
        picks = pick.pick(env(), lambda path: {"unexpected": True})
        self.assertEqual(set(picks.values()), {POOL})

    def test_outputs_are_json(self):
        for value in pick.pick(env(PICK_SLOTS="3"), fake({})).values():
            json.loads(value)


if __name__ == "__main__":
    unittest.main()
