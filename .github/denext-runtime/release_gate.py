#!/usr/bin/env python3
"""The release gates of denext_runtime.yml.

  release_gate.py tests <repo> <sha>
      The tag gate: may the commit <sha> be released? Reads every
      `denext tests` run of exactly that commit (push, dispatch, pull
      request: any event) and its jobs through `gh api`, prints why, and
      exits 1 when it may not. With GITHUB_OUTPUT set it also writes
      `full_run=<id>` and a `retried<<EOF` block (the jobs that only passed
      on a re-run attempt) for the release notes.

  release_gate.py latest <version> [<other release version>...]
      Prints `true` when <version> is the newest of all the versions given
      (semver precedence; build metadata ignored), else `false`: the
      release is marked GitHub "Latest" only then.

The tag gate's rule (see RULE below) exists because a green re-run used to
be enough: rc.3 and rc.6 shipped after an unexplained SIGABRT failed one run
of the same commit and a second run passed.
"""

import json
import os
import re
import subprocess
import sys

# The OS legs of a full `denext tests` run (denext_tests.yml): each reusable
# workflow call's jobs are named "<leg> / <job>". A branch push runs
# ubuntu-22.04 only, so its run is never "full".
LEGS = (
    "macos-14",
    "windows-2022",
    "ubuntu-22.04",
    "macos-15-intel",
    "ubuntu-22.04-arm",
    "windows-11-arm",
)

RULE = """\
The tag gate (denext_runtime.yml, release_gate.py):
  1. every `denext tests` run of the commit has finished;
  2. the NEWEST run that covers every OS leg (a full run) succeeded;
  3. no run of the commit, of any event (push or dispatch), has a job whose
     newest attempt failed, timed out or never started. A failed job only
     stops counting when that same run's job is re-run and passes
     (`gh run rerun <id> --failed`; the attempt is the job's run_attempt);
     a separate green run does not clear it, since that is exactly how a
     crash that fails one run in three gets released.
Jobs that passed only on a re-run are listed in the release notes."""

BAD = {"failure", "timed_out", "startup_failure"}


def leg_of(job_name):
    head, sep, _ = job_name.partition(" / ")
    return head if sep else None


def latest_jobs(jobs):
    """The newest attempt of each job (jobs from `filter=all`)."""
    newest = {}
    for job in jobs:
        prev = newest.get(job["name"])
        if prev is None or job.get("run_attempt", 1) >= prev.get("run_attempt", 1):
            newest[job["name"]] = job
    return newest


def is_full(jobs):
    ran = {
        leg_of(j["name"])
        for j in latest_jobs(jobs).values()
        if j.get("conclusion") not in (None, "skipped")
    }
    return all(leg in ran for leg in LEGS)


def evaluate(runs):
    """runs: [{id, event, head_branch, status, conclusion, run_attempt,
    created_at, jobs: [{name, conclusion, run_attempt}]}] (jobs of every
    attempt). Returns (errors, full_run_id, retried)."""
    errors = []
    retried = []
    for run in runs:
        where = f"run {run['id']} ({run['event']} on {run['head_branch']}, attempt {run.get('run_attempt', 1)})"
        if run.get("status") != "completed":
            errors.append(f"{where} has not finished ({run.get('status')}): wait for it")
            continue
        newest = latest_jobs(run["jobs"])
        bad = sorted(n for n, j in newest.items() if j.get("conclusion") in BAD)
        if bad:
            errors.append(
                f"{where}: job(s) failed and were not re-run green: {', '.join(bad)} "
                f"(find the cause, or `gh run rerun {run['id']} --failed` and wait for it)"
            )
        elif run.get("conclusion") in BAD:
            errors.append(f"{where} concluded {run['conclusion']} with no failed job listed")
        for name, job in sorted(newest.items()):
            if job.get("conclusion") == "success" and any(
                j["name"] == name and j.get("conclusion") in BAD for j in run["jobs"]
            ):
                retried.append(f"{name} (run {run['id']}, passed on attempt {job.get('run_attempt', 1)})")

    full = [r for r in runs if r.get("status") == "completed" and is_full(r["jobs"])]
    full.sort(key=lambda r: (r["created_at"], r["id"]), reverse=True)
    full_run = None
    if not full:
        errors.append(
            "no full 'denext tests' run (every OS leg: " + ", ".join(LEGS) + "): push the commit to "
            "development-denext or run 'gh workflow run denext_tests.yml --ref <branch> -f full=true'"
        )
    else:
        newest = full[0]
        if newest.get("conclusion") != "success":
            errors.append(
                f"the newest full run, {newest['id']} ({newest['event']}), concluded "
                f"{newest.get('conclusion')}, not success"
            )
        else:
            full_run = newest["id"]
    return errors, full_run, retried


def gh_lines(path):
    out = subprocess.run(
        ["gh", "api", "--paginate", path, "--jq", "(.workflow_runs // .jobs)[]"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def fetch(repo, sha):
    runs = gh_lines(f"repos/{repo}/actions/workflows/denext_tests.yml/runs?head_sha={sha}&per_page=100")
    for run in runs:
        run["jobs"] = gh_lines(f"repos/{repo}/actions/runs/{run['id']}/jobs?filter=all&per_page=100")
    return runs


def semver_key(version):
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?", version)
    if not m:
        return None
    core = tuple(int(x) for x in m.group(1, 2, 3))
    if m.group(4) is None:
        # A release sorts after every prerelease of the same core.
        return core + ((1,),)
    ids = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in m.group(4).split("."))
    return core + ((0,) + ids,)


def is_newest(version, others):
    mine = semver_key(version)
    if mine is None:
        raise SystemExit(f"::error::'{version}' is not a semver version")
    keys = [k for k in (semver_key(v) for v in others) if k is not None]
    return all(mine >= k for k in keys)


def main(argv):
    if len(argv) >= 3 and argv[0] == "tests":
        repo, sha = argv[1], argv[2]
        runs = fetch(repo, sha)
        print(RULE)
        print(f"\n'denext tests' runs of {sha}:")
        for r in runs:
            print(
                f"  {r['id']} {r['event']} {r['head_branch']} attempt {r.get('run_attempt', 1)}: "
                f"{r.get('status')}/{r.get('conclusion')}{' (full)' if is_full(r['jobs']) else ''}"
            )
        errors, full_run, retried = evaluate(runs)
        for line in retried:
            print(f"::warning::passed only on a re-run: {line}")
        if errors:
            for e in errors:
                print(f"::error::{e}")
            return 1
        print(f"\ndenext tests: the newest full run {full_run} passed, and no run of {sha} has an unretried failure")
        out = os.environ.get("GITHUB_OUTPUT")
        if out:
            with open(out, "a") as f:
                f.write(f"full_run={full_run}\n")
                f.write("retried<<RETRIED_EOF\n" + "\n".join(retried) + "\nRETRIED_EOF\n")
        return 0
    if len(argv) >= 2 and argv[0] == "latest":
        print("true" if is_newest(argv[1], argv[2:]) else "false")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
