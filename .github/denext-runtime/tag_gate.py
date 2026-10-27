#!/usr/bin/env python3
"""The release gate of denext_runtime.yml: may this commit be released?

A `denext-runtime-v*` tag publishes only when, for the tagged commit:

- a FULL `denext tests` run passed: every OS leg in REQUIRED_LEGS has a job
  that passed (a branch push runs Linux x64 only, and its other legs are
  skipped, so such a run never vouches for a release);
- NO `denext tests` run of the commit failed, timed out or failed to start,
  in any attempt, and none is still running. A crash that fails one run in
  three is a bug: re-running until one passes would ship it (the arm64 and
  macOS SIGABRTs of rc.3 and rc.6 did). A cancelled run (superseded by a
  newer push) is not a failure. The way past a red run is a new commit;
- the commit is on development-denext or on the release branch
  rel/<version>: a fix/* commit that was dispatched but never merged is not
  a release.

Usage: tag_gate.py
Env:   REPO (owner/name), SHA (the tagged commit), VERSION (the fork's version)
       and GH_TOKEN for the gh CLI.
Prints the full run's id as `full_run=<id>` and exits 0, or prints every
reason with an `::error::` prefix and exits 1.

Usage: tag_gate.py --prerelease-flag <version>
Prints `--prerelease` when the GitHub release of <version> is marked a
pre-release (see `is_prerelease`), else nothing.

Usage: tag_gate.py --latest <version> [<other release version>...]
Prints `true` when the release of <version> becomes the repository's
"Latest": it is not a pre-release and no other version given is newer (the
fork's order, `version_key`), else `false`.
"""

import json
import os
import re
import subprocess
import sys

WORKFLOW = "denext_tests.yml"
REQUIRED_LEGS = (
    "macos-15",
    "windows-2022",
    "ubuntu-22.04",
    "macos-15-intel",
    "ubuntu-22.04-arm",
    "windows-11-arm",
)
RED = ("failure", "timed_out", "startup_failure")
# A run (or attempt) GitHub concludes `failure` with no job that failed or
# timed out lost its jobs to the runner pool ("not acquired by Runner of type
# hosted"): GitHub cancels them and marks the run failed. No test failed, so it
# isn't red; it still never vouches, since only a green run with every leg does.
NO_RUNNER = "no_runner"


def classify(conclusion, job_conclusions):
    """The conclusion the gate counts for one attempt: `failure` becomes
    NO_RUNNER when none of the attempt's jobs failed or timed out."""
    if conclusion != "failure" or not job_conclusions:
        return conclusion
    if any(c in ("failure", "timed_out") for c in job_conclusions):
        return conclusion
    return NO_RUNNER
# `compare/<sha>...<branch>`: the branch contains the commit.
CONTAINED = ("ahead", "identical")


def release_branches(version):
    return ["development-denext", f"rel/{version}"]


def is_prerelease(version):
    """Whether the GitHub release of `version` is marked a pre-release: only
    a release candidate (`3.0.0-rc.N`). After its last release candidate the
    fork releases `3.0.0-denext.N`, its stable line, as full releases (the
    repository's "Latest"), and denext orders them after every `rc.N`.
    """
    pre = version.split("+", 1)[0].partition("-")[2]
    return pre == "rc" or pre.startswith("rc.")


# The fork's own-version line: on this core, every `denext.N` prerelease is
# newer than every `rc.N` one (semver's ASCII order would put `d` below `r`).
# denext orders the fork's versions the same way (compareDenoVersions).
OWN_LINE_CORE = (3, 0, 0)
OWN_LINE_RANK = {"rc": 0, "denext": 1}


def version_key(version):
    """A sort key for a release version, or None when it isn't semver.

    Semver precedence (build metadata ignored), except that on the 3.0.0
    core `3.0.0-rc.N` < `3.0.0-denext.N`: `2.9.7-denext.22` < `3.0.0-rc.1`
    < `3.0.0-rc.10` < `3.0.0-denext.1` < `3.0.0-denext.10` < `3.0.0`.
    """
    m = re.fullmatch(
        r"(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?",
        version,
    )
    if not m:
        return None
    core = tuple(int(x) for x in m.group(1, 2, 3))
    if m.group(4) is None:
        # A release sorts after every prerelease of the same core.
        return core + ((1,),)
    parts = m.group(4).split(".")
    rank = OWN_LINE_RANK.get(parts[0], -1) if core == OWN_LINE_CORE else -1
    ids = tuple(
        (0, int(p), "") if p.isdigit() else (1, 0, p) for p in parts
    )
    return core + ((0, rank) + ids,)


def is_latest(version, others):
    """Whether the release of `version` is marked the repository's "Latest":
    never a pre-release, and only when no other release is newer."""
    mine = version_key(version)
    if mine is None:
        raise SystemExit(f"::error::'{version}' is not a semver version")
    if is_prerelease(version):
        return False
    keys = [k for k in (version_key(v) for v in others) if k is not None]
    return all(mine >= k for k in keys)


def evaluate(runs, legs_of, contained_in):
    """The verdict for one commit.

    runs: every `denext tests` run of the commit, each a dict with `id`,
      `status`, `conclusion` and `attempts` (the conclusions of its earlier
      attempts, oldest first; empty for a run never re-run).
    legs_of: run id -> the OS legs that have a passed job in that run.
    contained_in: branch name -> whether it contains the commit.
    Returns (full_run_id or None, [errors]).
    """
    errors = []
    pending = [r["id"] for r in runs if r["status"] != "completed"]
    if pending:
        errors.append(
            "denext tests runs still in progress for this commit: "
            + ", ".join(map(str, pending))
            + " (wait for them before tagging)"
        )
    red = []
    for r in runs:
        conclusions = list(r.get("attempts", [])) + [r["conclusion"]]
        for attempt, conclusion in enumerate(conclusions, 1):
            if conclusion in RED:
                red.append(f"{r['id']} (attempt {attempt}: {conclusion})")
    if red:
        errors.append(
            "denext tests failed on this commit: "
            + ", ".join(red)
            + ". A re-run that passes does not clear a failure: fix it (or"
            " name the flake and its fix) in a new commit and tag that"
        )
    full_run = None
    for r in sorted(runs, key=lambda r: r["id"]):
        if r["status"] != "completed" or r["conclusion"] != "success":
            continue
        legs = set(legs_of(r["id"]))
        missing = [leg for leg in REQUIRED_LEGS if leg not in legs]
        if not missing:
            full_run = r["id"]
            break
        print(f"denext tests run {r['id']} lacks: {' '.join(missing)}")
    if full_run is None:
        errors.append(
            "no successful full 'denext tests' run (legs: "
            + ", ".join(REQUIRED_LEGS)
            + ") for this commit: push it to development-denext (or run 'gh"
            " workflow run denext_tests.yml --ref <branch> -f full=true') and"
            " wait for it to pass before tagging"
        )
    if not any(contained_in.values()):
        errors.append(
            "the commit is on none of "
            + ", ".join(contained_in)
            + ": land it (fast-forward development-denext, or push the"
            " release branch) before tagging"
        )
    return full_run, errors


def gh_lines(args):
    out = subprocess.run(
        ["gh", "api", *args], check=True, capture_output=True, text=True
    ).stdout
    return [line for line in out.splitlines() if line.strip()]


def gh_runs(repo, sha):
    runs = []
    for line in gh_lines(
        [
            "--paginate",
            f"repos/{repo}/actions/workflows/{WORKFLOW}/runs"
            f"?head_sha={sha}&per_page=100",
            "--jq",
            ".workflow_runs[] | {id, status, conclusion, run_attempt}",
        ]
    ):
        run = json.loads(line)
        # A re-run replaces the run's conclusion: read each earlier attempt
        # (`--jq` prints a string unquoted; a null conclusion prints "null").
        last = int(run.get("run_attempt") or 1)
        run["attempts"] = [
            attempt_conclusion(
                repo,
                run["id"],
                n,
                gh_lines(
                    [
                        f"repos/{repo}/actions/runs/{run['id']}/attempts/{n}",
                        "--jq",
                        ".conclusion",
                    ]
                )[0],
            )
            for n in range(1, last)
        ]
        run["conclusion"] = attempt_conclusion(
            repo, run["id"], last, run["conclusion"]
        )
        runs.append(run)
    return runs


def attempt_conclusion(repo, run_id, attempt, conclusion):
    if conclusion != "failure":
        return conclusion
    jobs = gh_lines(
        [
            "--paginate",
            f"repos/{repo}/actions/runs/{run_id}/attempts/{attempt}/jobs"
            "?per_page=100",
            "--jq",
            ".jobs[] | .conclusion",
        ]
    )
    return classify(conclusion, jobs)


def gh_legs(repo, run_id):
    # "Leg / job" names: the leg is the reusable workflow's caller job name.
    names = gh_lines(
        [
            "--paginate",
            f"repos/{repo}/actions/runs/{run_id}/jobs?per_page=100",
            "--jq",
            '.jobs[] | select(.conclusion == "success") | .name',
        ]
    )
    return {name.split(" / ", 1)[0] for name in names if " / " in name}


def gh_contains(repo, sha, branch):
    result = subprocess.run(
        [
            "gh",
            "api",
            f"repos/{repo}/compare/{sha}...{branch}",
            "--jq",
            ".status",
        ],
        capture_output=True,
        text=True,
    )
    # A missing branch (404) contains nothing.
    return result.returncode == 0 and result.stdout.strip() in CONTAINED


def main():
    env = os.environ
    repo, sha, version = env["REPO"], env["SHA"], env["VERSION"]
    runs = gh_runs(repo, sha)
    contained_in = {
        branch: gh_contains(repo, sha, branch)
        for branch in release_branches(version)
    }
    full_run, errors = evaluate(
        runs, lambda run_id: gh_legs(repo, run_id), contained_in
    )
    for error in errors:
        print(f"::error::{error}")
    if errors:
        return 1
    print(f"denext tests (full matrix, run {full_run}) passed for {sha}")
    print(f"full_run={full_run}")
    return 0


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--prerelease-flag":
        print("--prerelease" if is_prerelease(sys.argv[2]) else "")
        sys.exit(0)
    if len(sys.argv) >= 3 and sys.argv[1] == "--latest":
        print("true" if is_latest(sys.argv[2], sys.argv[3:]) else "false")
        sys.exit(0)
    sys.exit(main())
