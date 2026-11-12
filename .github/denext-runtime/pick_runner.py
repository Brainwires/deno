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
- PICK_SHARED_LABELS: other pools' labels (space separated) whose jobs take
  the same slots: biscuits' linux-x64, linux-x64-2204 and linux-x64-docker
  pools share one capacity.
- PICK_FORK_PR: `true` on a fork's pull request: every job takes PICK_IMAGE
  (a self-hosted runner never takes a fork's code), with no API call.
- PICK_OWN_SKIP: job-name fragments (`|` separated) of this run's own jobs
  that wait on this pick and so hold no slot yet (" / test node_compat").
- GITHUB_TOKEN (`actions: read`), GITHUB_REPOSITORY, GITHUB_RUN_ID,
  GITHUB_API_URL.
- PICK_HEALTH (optional): the organization variable DENEXT_POOL_HEALTH, which
  the runner fleet's watchdog (ops/runners/watchdog.sh in denext-internal)
  keeps: {"v": 1, "checked_at": <epoch>, "pools": {"<label>": {"ok": bool,
  ...}}, ...}. A pool whose `ok` is false gets no slot (every job takes
  PICK_IMAGE). A missing, unparsable or stale value (checked_at more than 30
  minutes old), or a pool it does not list, changes nothing.
- PICK_IMAGE_KEYS (optional): keys that always take PICK_IMAGE, whatever the
  pool's slots (a suite the pool's runners cannot pass by design, such as
  Node's network tests on a runner whose firewall isolates it from the LAN).
  They hold no slot.
- RUNNER_STATUS_TOKEN (optional): a token that may list the organization's
  self-hosted runners. With it a runner of ours busy with another
  repository's job (denext's CI shares the pools) counts too.

A slot is taken by any queued or running job, in this repository, that names
the pool's label: the scheduler starts a just-in-time runner per queued job.
On any failure (the API, a timeout, a malformed answer) every job takes the
pool: the routing it had before this script.

A re-run (GITHUB_RUN_ATTEMPT > 1) whose previous attempt had a job on the pool
that was cancelled before any runner took it sends every job to PICK_IMAGE:
the pool was unhealthy. That is how the runner fleet's watchdog
(ops/runners/watchdog.sh in denext-internal) re-routes a job stuck on a dead
pool: it cancels the run (never on a default branch) and re-runs this picker's
job, which then overflows to GitHub. A failure of that check is ignored.
"""

import json
import os
import sys
import time
import urllib.request

ACTIVE = {"queued", "in_progress", "waiting", "pending", "requested"}
TIMEOUT = 20  # seconds, per request
MAX_PAGES = 10
HEALTH_MAX_AGE = 30 * 60  # seconds: an older DENEXT_POOL_HEALTH is ignored


def fresh_pools(value, now=None):
    """DENEXT_POOL_HEALTH's `pools` map ({label: entry}) when `value` (schema
    v1) parses and is fresh, else None (missing, unparsable or stale: the
    pickers behave as without it). plan_matrix.py reads it too."""
    try:
        health = json.loads(value or "")
    except ValueError:
        return None
    if not isinstance(health, dict):
        return None
    at = health.get("checked_at")
    if isinstance(at, bool) or not isinstance(at, (int, float)):
        return None
    age = (time.time() if now is None else now) - at
    if age > HEALTH_MAX_AGE or age < -300:
        return None
    pools = health.get("pools")
    return pools if isinstance(pools, dict) else None


def pool_unhealthy(value, label, now=None):
    """True only when `value` (DENEXT_POOL_HEALTH, schema v1) is fresh and its
    pools[label].ok is false. Anything else (missing, unparsable, stale, the
    label absent, ok true) is False: the picker behaves as without it."""
    pools = fresh_pools(value, now)
    pool = pools.get(label) if pools is not None else None
    return isinstance(pool, dict) and pool.get("ok") is False


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


def _labels(label):
    return {label} if isinstance(label, str) else set(label)


def active_on(jobs, label, skip=()):
    """How many of `jobs` hold (or will hold) a slot of the pool `label` (a
    label, or the set of labels that share its slots)."""
    want = _labels(label)
    return sum(
        1
        for j in jobs
        if j.get("status") in ACTIVE
        and want & set(j.get("labels") or [])
        and not any(s in (j.get("name") or "") for s in skip)
    )


def busy_runners(runners, label):
    want = _labels(label)
    return sum(
        1
        for r in runners
        if r.get("status") == "online"
        and r.get("busy")
        and any(l.get("name") in want for l in r.get("labels") or [])
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


def previous_attempt_stalled(get, repo, run_id, attempt, label):
    """True on a re-run whose previous attempt had a job on the pool `label`
    that ended cancelled with no runner (it never left the queue)."""
    try:
        n = int(attempt or "1")
    except ValueError:
        return False
    if n <= 1:
        return False
    want = _labels(label)
    jobs = list_all(
        get, f"/repos/{repo}/actions/runs/{run_id}/attempts/{n - 1}/jobs", "jobs"
    )
    return any(
        j.get("conclusion") == "cancelled"
        and not j.get("runner_name")
        and want & set(j.get("labels") or [])
        for j in jobs
    )


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
    """PICK_IMAGE_KEYS' jobs take the image; the others are picked by
    pick_pool."""
    keys = (env.get("PICK_KEYS") or "").split()
    image_only = set((env.get("PICK_IMAGE_KEYS") or "").split())
    pooled = [k for k in keys if k not in image_only]
    out = (
        pick_pool({**env, "PICK_KEYS": " ".join(pooled)}, get, runner_get, warn)
        if pooled
        else {}
    )
    hosted = json.dumps(env.get("PICK_IMAGE") or "ubuntu-latest")
    return {k: (hosted if k in image_only else out[k]) for k in keys}


def pick_pool(env, get=None, runner_get=None, warn=None):
    keys = (env.get("PICK_KEYS") or "").split()
    image = env.get("PICK_IMAGE") or "ubuntu-latest"
    ours, label = parse_runner(env.get("PICK_RUNNER", ""))
    try:
        slots = max(0, int(env.get("PICK_SLOTS") or "1"))
    except ValueError:
        slots = 1
    if not ours or env.get("PICK_FORK_PR") == "true":
        return allocate(keys, None, image, 0)
    if pool_unhealthy(env.get("PICK_HEALTH"), label):
        if warn:
            warn(
                f"pick_runner: DENEXT_POOL_HEALTH marks {label} unhealthy;"
                " every job takes the image"
            )
        return allocate(keys, ours, image, 0)
    skip = tuple(s for s in (env.get("PICK_OWN_SKIP") or "").split("|") if s)
    labels = {label, *(env.get("PICK_SHARED_LABELS") or "").split()}
    try:
        if previous_attempt_stalled(
            get,
            env.get("GITHUB_REPOSITORY", ""),
            env.get("GITHUB_RUN_ID", ""),
            env.get("GITHUB_RUN_ATTEMPT", "1"),
            {label},
        ):
            if warn:
                warn(
                    "pick_runner: the previous attempt's job on the pool was cancelled"
                    " before a runner took it (the pool was unhealthy); every job"
                    " takes the image"
                )
            return allocate(keys, ours, image, 0)
    except Exception as e:  # the check is advisory: fall through to the slot count
        if warn:
            warn(f"pick_runner: previous-attempt check failed: {e}; ignored")
    try:
        busy = busy_slots(
            get,
            env.get("GITHUB_REPOSITORY", ""),
            env.get("GITHUB_RUN_ID", ""),
            labels,
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
