#!/usr/bin/env python3
"""The verified-SHA guard of the workflows that run on a push to
development-denext: did this exact commit already pass this workflow?

After a release, development-denext is fast-forwarded to the release commit,
and that push starts `denext tests`, `denext runtime` (and, when its paths
match, `denext npm matrix`) again on a commit that already passed them on its
release branch and tag. The guard job runs first; when it prints
`verified=true`, every other job of the run is skipped and the run concludes
green on the guard alone. Otherwise the run goes ahead as normal.

A commit counts as verified only by a completed, successful run of the SAME
workflow on the SAME commit (`head_sha`), another run than this one, from
another event or ref, that covered everything this run would:

- denext_tests.yml: a `workflow_dispatch` on `rel/*` whose jobs show every OS
  leg of tag_gate.py's REQUIRED_LEGS passed (a full=true run; the API does
  not return a dispatch's inputs, so its legs are the proof).
- denext_runtime.yml: the `denext-runtime-v*` tag build (it built, tested and
  published every target), or a `workflow_dispatch` or push on `rel/*` whose
  `laufey / `, `denort / `, `cli / ` and `package / ` jobs passed for every
  target in tag_gate.py's LAUFEY_TARGETS (not a cli_only, reuse or
  targets-filtered run).
- denext_npm_matrix.yml: a push on `rel/*` (a push runs every case; a
  dispatch may narrow them with `only`, which the API does not return).

And never when any run of this workflow on the commit failed, timed out or
failed to start (tag_gate.py's RED): the full run goes ahead and shows it.

Never a false green: any error reading the API (a failed `gh` call, output
that does not parse) prints a warning and `verified=false`, so the run goes
ahead in full. The script always exits 0 unless its arguments are wrong.

Usage: guard_verified.py <workflow file name>
Env:   REPO (owner/name), SHA (the pushed commit), EVENT (github.event_name),
       REF (github.ref), RUN_ID (this run, left out) and GH_TOKEN for the gh
       CLI.
Prints the reason, then `verified=true` or `verified=false` (the line the
workflow appends to $GITHUB_OUTPUT).
"""

import importlib.util
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))

_spec = importlib.util.spec_from_file_location(
    "tag_gate", os.path.join(HERE, "tag_gate.py")
)
tag_gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tag_gate)

GUARDED_REF = "refs/heads/development-denext"
TESTS = "denext_tests.yml"
RUNTIME = "denext_runtime.yml"
NPM_MATRIX = "denext_npm_matrix.yml"
WORKFLOWS = (TESTS, RUNTIME, NPM_MATRIX)
RUNTIME_TAG_PREFIX = "denext-runtime-v"
# The per-target jobs a full `denext runtime` run passes for every target.
RUNTIME_JOBS = ("laufey", "denort", "cli", "package")


def is_guarded(event, ref):
    """Only a push to development-denext is ever skipped."""
    return event == "push" and ref == GUARDED_REF


def passed_jobs(jobs):
    """The names of the jobs that passed. jobs: dicts with `name` and
    `conclusion`."""
    return {j.get("name") or "" for j in jobs if j.get("conclusion") == "success"}


def full_tests_run(jobs):
    """Whether a `denext tests` run passed every OS leg (`<leg> / <job>`)."""
    legs = {
        name.split(" / ", 1)[0] for name in passed_jobs(jobs) if " / " in name
    }
    return all(leg in legs for leg in tag_gate.REQUIRED_LEGS)


def full_runtime_run(jobs):
    """Whether a `denext runtime` run passed each per-target job for every
    target."""
    names = passed_jobs(jobs)
    return all(
        f"{job} / {target}" in names
        for job in RUNTIME_JOBS
        for target in tag_gate.LAUFEY_TARGETS
    )


def vouches(workflow, run, jobs_of):
    """Whether one other successful run of the commit verifies it."""
    event = run.get("event")
    branch = run.get("head_branch") or ""
    if workflow == TESTS:
        return (
            event == "workflow_dispatch"
            and branch.startswith("rel/")
            and full_tests_run(jobs_of(run["id"]))
        )
    if workflow == RUNTIME:
        if event == "push" and branch.startswith(RUNTIME_TAG_PREFIX):
            return True
        return (
            event in ("workflow_dispatch", "push")
            and branch.startswith("rel/")
            and full_runtime_run(jobs_of(run["id"]))
        )
    if workflow == NPM_MATRIX:
        return event == "push" and branch.startswith("rel/")
    raise ValueError(f"unknown workflow {workflow}")


def decide(workflow, sha, own_run, runs, jobs_of):
    """(verified, reason) for one commit.

    runs: the workflow's runs of the commit, each a dict with `id`, `event`,
      `head_branch`, `head_sha`, `status` and `conclusion`.
    jobs_of: run id -> that run's jobs (see passed_jobs); it may raise.
    """
    runs = [
        r
        for r in runs
        if r.get("head_sha") == sha and str(r.get("id")) != str(own_run or "")
    ]
    red = [r["id"] for r in runs if r.get("conclusion") in tag_gate.RED]
    if red:
        return False, (
            f"{workflow} failed on {sha} before (runs "
            + ", ".join(map(str, red))
            + "): running in full"
        )
    for r in sorted(runs, key=lambda r: r["id"]):
        if r.get("status") != "completed" or r.get("conclusion") != "success":
            continue
        if vouches(workflow, r, jobs_of):
            return True, (
                f"{workflow} already passed on {sha}: run {r['id']}"
                f" ({r.get('event')} on {r.get('head_branch')}); skipping"
                " this run's jobs"
            )
    return False, (
        f"no earlier successful {workflow} run verifies {sha}: running in full"
    )


def gh_lines(args):
    out = subprocess.run(
        ["gh", "api", *args], check=True, capture_output=True, text=True
    ).stdout
    return [line for line in out.splitlines() if line.strip()]


def gh_runs(repo, workflow, sha):
    return [
        json.loads(line)
        for line in gh_lines(
            [
                "--paginate",
                f"repos/{repo}/actions/workflows/{workflow}/runs"
                f"?head_sha={sha}&per_page=100",
                "--jq",
                ".workflow_runs[]"
                " | {id, event, head_branch, head_sha, status, conclusion}",
            ]
        )
    ]


def gh_jobs(repo, run_id):
    return [
        json.loads(line)
        for line in gh_lines(
            [
                "--paginate",
                f"repos/{repo}/actions/runs/{run_id}/jobs?per_page=100",
                "--jq",
                ".jobs[] | {name, conclusion}",
            ]
        )
    ]


def check(workflow, env, runs_of=gh_runs, jobs_of=gh_jobs):
    """(verified, reason); any error reading the API is `verified=false`."""
    if not is_guarded(env.get("EVENT"), env.get("REF")):
        return False, (
            f"{env.get('EVENT')} on {env.get('REF')} is not a push to"
            " development-denext: running in full"
        )
    repo, sha = env.get("REPO", ""), env.get("SHA", "")
    if not repo or not sha:
        return False, "::warning::REPO or SHA is unset: running in full"
    try:
        return decide(
            workflow,
            sha,
            env.get("RUN_ID", ""),
            runs_of(repo, workflow, sha),
            lambda run_id: jobs_of(repo, run_id),
        )
    except Exception as e:  # noqa: BLE001 - any failure means a full run
        detail = str(e).strip().replace("\n", " ")
        if isinstance(e, subprocess.CalledProcessError) and e.stderr:
            detail += " " + e.stderr.strip().replace("\n", " ")
        return False, (
            "::warning::the verified-SHA guard could not read the workflow"
            f" runs ({type(e).__name__}: {detail}): running in full"
        )


def main(argv, env):
    if len(argv) != 2 or argv[1] not in WORKFLOWS:
        print(
            f"usage: guard_verified.py <{'|'.join(WORKFLOWS)}>", file=sys.stderr
        )
        return 2
    verified, reason = check(argv[1], env)
    print(reason)
    print(f"verified={'true' if verified else 'false'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv, os.environ))
