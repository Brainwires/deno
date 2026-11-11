#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Picks the runner of each job that may run on one of our self-hosted pools:
the pool while it has a free slot, GitHub's image for the overflow.

denext_tests_os.yml runs it (the `node-runner` job) right after a leg's build,
before the leg's unit_node and node_compat jobs start, so the pick sees the
pool as it is when they queue. Each job reads its choice from
`needs.node-runner.outputs.<key>` and falls back to the pool when the output is
empty (this script failed before it printed).

    python3 -I .github/denext-runtime/pick_runner.py >> "$GITHUB_OUTPUT"

Prints one `<key>=<runs-on JSON>` line per job key. Environment:

- PICK_KEYS: the job keys in priority order (space separated): the first ones
  take the free slots.
- PICK_RUNNER: the pool, as its repository variable holds it: a label, or a
  JSON array of labels (["self-hosted","denext-macos-arm64"]). Empty: every
  job takes PICK_IMAGE.
- PICK_IMAGE: GitHub's image for the overflow (the leg's `os`).
- PICK_SLOTS: how many jobs the pool runs at once (default 1).
- PICK_FORK_PR: `true` on a fork's pull request: every job takes PICK_IMAGE
  (a self-hosted runner never takes a fork's code), with no API call.
- PICK_OWN_SKIP: job-name fragments (`|` separated) of this run's own jobs
  that wait on this pick and so hold no slot yet (" / test node_compat").
- GITHUB_TOKEN (`actions: read`), GITHUB_REPOSITORY, GITHUB_RUN_ID,
  GITHUB_API_URL.
- RUNNER_STATUS_TOKEN (optional): a token that may list the organization's
  self-hosted runners. With it a runner of ours busy with another
  repository's job (denext's CI shares the pools) counts too.

A slot is taken by any queued or running job, in this repository, that names
the pool's label: the scheduler starts a just-in-time runner per queued job.
On any failure (the API, a timeout, a malformed answer) every job takes the
pool: the routing it had before this script.
"""

import json
import os
import sys
import urllib.request

ACTIVE = {"queued", "in_progress", "waiting", "pending", "requested"}
TIMEOUT = 20  # seconds, per request
MAX_PAGES = 10


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


def active_on(jobs, label, skip=()):
    """How many of `jobs` hold (or will hold) a slot of the pool `label`."""
    return sum(
        1
        for j in jobs
        if j.get("status") in ACTIVE
        and label in (j.get("labels") or [])
        and not any(s in (j.get("name") or "") for s in skip)
    )


def busy_runners(runners, label):
    return sum(
        1
        for r in runners
        if r.get("status") == "online"
        and r.get("busy")
        and any(l.get("name") == label for l in r.get("labels") or [])
    )


def allocate(keys, ours, image, free):
    """{key: runs-on JSON}: the pool for the first `free` keys, the image for
    the rest."""
    hosted = json.dumps(image)
    left = max(0, free) if ours else 0
    out = {}
    for key in keys:
        out[key] = ours if left > 0 else hosted
        left -= 1
    return out


def api(base, token):
    def get(path):
        req = urllib.request.Request(
            base + path,
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {token}",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        with urllib.request.urlopen(req, timeout=TIMEOUT) as res:
            return json.load(res)

    return get


def list_all(get, path, key):
    items = []
    for page in range(1, MAX_PAGES + 1):
        sep = "&" if "?" in path else "?"
        body = get(f"{path}{sep}per_page=100&page={page}")
        batch = body.get(key) if isinstance(body, dict) else None
        if not isinstance(batch, list):
            raise ValueError(f'no "{key}" in the answer to {path}')
        items += batch
        if len(batch) < 100:
            break
    return items


def busy_slots(get, repo, run_id, label, skip, runner_get=None):
    """The pool's taken slots: this repository's active jobs that name its
    label (this run's own jobs that wait on the pick left out), and with
    `runner_get` at least as many as its busy runners."""
    runs = []
    for status in ("queued", "in_progress"):
        runs += list_all(get, f"/repos/{repo}/actions/runs?status={status}", "workflow_runs")
    running = queued = 0
    seen = set()
    for run in runs:
        if run["id"] in seen:
            continue
        seen.add(run["id"])
        jobs = list_all(
            get, f"/repos/{repo}/actions/runs/{run['id']}/jobs?filter=latest", "jobs"
        )
        own = str(run["id"]) == str(run_id)
        skip_here = skip if own else ()
        running += active_on(
            [j for j in jobs if j.get("status") == "in_progress"], label, skip_here
        )
        queued += active_on(
            [j for j in jobs if j.get("status") != "in_progress"], label, skip_here
        )
    if runner_get is not None:
        org = repo.split("/")[0]
        runners = list_all(runner_get, f"/orgs/{org}/actions/runners", "runners")
        running = max(running, busy_runners(runners, label))
    return running + queued


def pick(env, get=None, runner_get=None, warn=None):
    keys = (env.get("PICK_KEYS") or "").split()
    image = env.get("PICK_IMAGE") or "ubuntu-latest"
    ours, label = parse_runner(env.get("PICK_RUNNER", ""))
    try:
        slots = max(0, int(env.get("PICK_SLOTS") or "1"))
    except ValueError:
        slots = 1
    if not ours or env.get("PICK_FORK_PR") == "true":
        return allocate(keys, None, image, 0)
    skip = tuple(s for s in (env.get("PICK_OWN_SKIP") or "").split("|") if s)
    try:
        busy = busy_slots(
            get,
            env.get("GITHUB_REPOSITORY", ""),
            env.get("GITHUB_RUN_ID", ""),
            label,
            skip,
            runner_get,
        )
    except Exception as e:  # any failure: the pool, as before this script
        if warn:
            warn(f"pick_runner: {e}; every job keeps the pool")
        return {key: ours for key in keys}
    return allocate(keys, ours, image, slots - busy)


def main():
    env = os.environ
    base = env.get("GITHUB_API_URL") or "https://api.github.com"
    get = api(base, env.get("GITHUB_TOKEN", ""))
    runner_token = env.get("RUNNER_STATUS_TOKEN")
    picks = pick(
        env,
        get,
        api(base, runner_token) if runner_token else None,
        lambda m: print(f"::warning::{m}", file=sys.stderr),
    )
    for key, runs_on in picks.items():
        print(f"{key}={runs_on}")
        print(f"{key}: {runs_on}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
