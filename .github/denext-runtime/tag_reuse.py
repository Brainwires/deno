#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Which builds a release tag's run of denext_runtime.yml takes from an
earlier run of the same commit instead of building them again.

A tag build (denext-runtime-v<version>) builds laufey, libdenort, denort and
the CLI for every target, hours on the slowest pool, although the commit was
usually built minutes before. The setup job of a tag run asks this script for
an earlier run whose builds it may take; the jobs it names are skipped and
their artifacts downloaded from that run (download-artifact `run-id`). The
package, e2e_fork and rpm-smoke jobs still run on what is published, and the
publish job attests every published file again in the tag run.

A run's builds are taken only when they are the same bytes a tag build would
make:

- the same repository (never a fork: run and head repository), the same
  commit (`head_sha`), and the same workflow file: the run's `path` is
  denext_runtime.yml and the file's git blob at the run's commit equals the
  blob at the tag's commit;
- a branch push (rel/** or development-denext) or a dispatch, never another
  tag's run, and never this run;
- its plan marker (the `plan-v1_...` artifact every setup job uploads) says
  it was built with the release's own version (a dispatch with
  `release_version`: no `+ci.N` build metadata, which `deno --version`, the
  archive names and denort's DENEXT_VERSION carry) and without a build cache
  (`cache=off`: a cache any branch or pull request run could have written
  must not reach a signed release, as for a tag build itself), and, for
  laufey and libdenort, from the same laufey commit;
- for a job kind, that job passed for every target and its artifacts are
  there and not expired (with the symbol archives the release attaches).

The CLI embeds the SHA-256 of every target's denort archive
(DENEXT_DENORT_SHA256S), so the CLI is taken only together with denort bin
from the same run; denort bin alone may be taken while the CLI builds here
on it. A plain rel/** push never qualifies (it stamps `+ci.N` and builds
with the cache): the way to a fast tag is a `release_version` dispatch of
the release commit, then the tag.

Never a wrong reuse: any error reading the API prints a warning and reuses
nothing, so the tag builds everything. The script exits 0 unless its
arguments are wrong.

Env: REPO, SHA, RUN_ID (this run), VERSION (the release's), LAUFEY_SHA,
     TARGETS (comma-separated, the release's), GH_TOKEN for the gh CLI.
Prints the reasons, then one `reuse_<kind>=<run id or empty>` line per kind
(the lines the workflow appends to $GITHUB_OUTPUT).
"""

import json
import os
import subprocess
import sys

WORKFLOW = "denext_runtime.yml"
WORKFLOW_PATH = ".github/workflows/denext_runtime.yml"
TAG_PREFIX = "denext-runtime-v"
PLAN_PREFIX = "plan-v1_"

# kind: (job name prefix, artifact name prefixes, needs the same laufey)
KINDS = {
    "laufey": ("laufey", ("laufey",), True),
    "denort": ("denort", ("denort", "symbols-denort"), True),
    "denort_bin": ("denort bin", ("denort-bin", "symbols-denort-bin"), False),
    "cli": ("cli", ("cli", "symbols-cli"), False),
}
ORDER = ("laufey", "denort", "denort_bin", "cli")


def plan_marker(version, laufey_sha, cache):
    """The plan artifact's name for a run (the setup job uploads it)."""
    return f"{PLAN_PREFIX}version={version}_laufey={laufey_sha}_cache={cache}"


def parse_marker(name):
    """{version, laufey, cache} from a plan artifact name, or None."""
    if not name.startswith(PLAN_PREFIX):
        return None
    out = {}
    for part in name[len(PLAN_PREFIX):].split("_"):
        key, sep, value = part.partition("=")
        if not sep:
            return None
        out[key] = value
    if not all(k in out for k in ("version", "laufey", "cache")):
        return None
    return out


def eligible(run, want):
    """(ok, reason) for the run itself, before its jobs and artifacts."""
    if str(run.get("id")) == str(want["run_id"]):
        return False, "this run"
    if run.get("head_sha") != want["sha"]:
        return False, "another commit"
    if run.get("repository") != want["repo"] or run.get("head_repository") != want["repo"]:
        return False, "another repository"
    if run.get("path") != WORKFLOW_PATH:
        return False, f"workflow file {run.get('path')}"
    event, branch = run.get("event"), run.get("head_branch") or ""
    if event == "workflow_dispatch":
        pass
    elif event == "push" and (branch.startswith("rel/") or branch == "development-denext"):
        pass
    else:
        return False, f"{event} on {branch}"
    return True, ""


def marker_of(artifacts):
    for a in artifacts:
        m = parse_marker(a.get("name") or "")
        if m is not None:
            return m
    return None


def kind_ok(kind, jobs, artifacts, targets):
    """(ok, reason) for one job kind of a run, every target."""
    job, prefixes, _ = KINDS[kind]
    passed = {j.get("name") for j in jobs if j.get("conclusion") == "success"}
    live = {a.get("name") for a in artifacts if not a.get("expired")}
    for t in targets:
        if f"{job} / {t}" not in passed:
            return False, f"`{job} / {t}` did not pass"
        for p in prefixes:
            if f"{p}-{t}" not in live:
                return False, f"no artifact {p}-{t}"
    return True, ""


def select(runs, details, want):
    """({kind: run id or None}, [reasons]).

    runs: the workflow's runs of the commit (dicts with id, event,
      head_branch, head_sha, path, repository, head_repository).
    details: run id -> {"jobs": [...], "artifacts": [...], "blob": str}; it
      may raise.
    want: sha, repo, run_id, version, laufey_sha, targets, blob.
    """
    picks = {k: None for k in ORDER}
    reasons = []
    for run in sorted(runs, key=lambda r: int(r["id"]), reverse=True):
        rid = run["id"]
        ok, why = eligible(run, want)
        if not ok:
            reasons.append(f"run {rid}: not a candidate ({why})")
            continue
        d = details(rid)
        if d["blob"] != want["blob"]:
            reasons.append(f"run {rid}: another {WORKFLOW_PATH} blob")
            continue
        m = marker_of(d["artifacts"])
        if m is None:
            reasons.append(f"run {rid}: no plan marker")
            continue
        if m["version"] != want["version"]:
            reasons.append(f"run {rid}: built as {m['version']}, not {want['version']}")
            continue
        if m["cache"] != "off":
            reasons.append(f"run {rid}: built with the build cache")
            continue
        found = {}
        for kind in ORDER:
            if KINDS[kind][2] and m["laufey"] != want["laufey_sha"]:
                found[kind] = (False, f"laufey {m['laufey']}")
                continue
            found[kind] = kind_ok(kind, d["jobs"], d["artifacts"], want["targets"])
        # The CLI only with the denort archives it embeds: from the run its
        # denort bin comes from, picked in the same step.
        if found["cli"][0] and not found["denort_bin"][0]:
            found["cli"] = (False, "its denort bin is not reusable")
        for kind in ("laufey", "denort", "denort_bin"):
            if picks[kind] is not None:
                continue
            ok, why = found[kind]
            if not ok:
                reasons.append(f"run {rid}: not reusing {kind} ({why})")
                continue
            picks[kind] = rid
            reasons.append(f"run {rid}: reusing {kind}")
            if kind == "denort_bin":
                ok, why = found["cli"]
                if ok:
                    picks["cli"] = rid
                    reasons.append(f"run {rid}: reusing cli")
                else:
                    reasons.append(f"run {rid}: not reusing cli ({why}); it builds on this denort bin")
        if all(picks[k] is not None for k in ("laufey", "denort", "denort_bin")):
            break
    return picks, reasons


def gh_lines(args):
    out = subprocess.run(
        ["gh", "api", *args], check=True, capture_output=True, text=True
    ).stdout
    return [line for line in out.splitlines() if line.strip()]


def gh_runs(repo, sha):
    return [
        json.loads(line)
        for line in gh_lines([
            "--paginate",
            f"repos/{repo}/actions/workflows/{WORKFLOW}/runs?head_sha={sha}&per_page=100",
            "--jq",
            ".workflow_runs[] | {id, event, head_branch, head_sha, path,"
            " repository: .repository.full_name,"
            " head_repository: .head_repository.full_name}",
        ])
    ]


def gh_blob(repo, sha):
    (line,) = gh_lines([f"repos/{repo}/contents/{WORKFLOW_PATH}?ref={sha}", "--jq", ".sha"])
    return line.strip()


def gh_details(repo, run_id, head_sha, blobs):
    if head_sha not in blobs:
        blobs[head_sha] = gh_blob(repo, head_sha)
    jobs = [
        json.loads(line)
        for line in gh_lines([
            "--paginate", f"repos/{repo}/actions/runs/{run_id}/jobs?filter=latest&per_page=100",
            "--jq", ".jobs[] | {name, conclusion}",
        ])
    ]
    artifacts = [
        json.loads(line)
        for line in gh_lines([
            "--paginate", f"repos/{repo}/actions/runs/{run_id}/artifacts?per_page=100",
            "--jq", ".artifacts[] | {name, expired}",
        ])
    ]
    return {"jobs": jobs, "artifacts": artifacts, "blob": blobs[head_sha]}


def check(env, runs_of=gh_runs, details_of=gh_details, blob_of=gh_blob):
    """({kind: id or None}, reasons); any API error reuses nothing."""
    none = {k: None for k in ORDER}
    want = {
        "repo": env.get("REPO", ""),
        "sha": env.get("SHA", ""),
        "run_id": env.get("RUN_ID", ""),
        "version": env.get("VERSION", ""),
        "laufey_sha": env.get("LAUFEY_SHA", ""),
        "targets": [t.strip() for t in (env.get("TARGETS") or "").split(",") if t.strip()],
    }
    if not all(want[k] for k in ("repo", "sha", "version", "laufey_sha", "targets")):
        return none, ["::warning::tag_reuse: REPO, SHA, VERSION, LAUFEY_SHA or TARGETS is unset: building everything"]
    try:
        blobs = {}
        want["blob"] = blobs[want["sha"]] = blob_of(want["repo"], want["sha"])
        runs = runs_of(want["repo"], want["sha"])
        by_id = {r["id"]: r for r in runs}
        return select(
            runs,
            lambda rid: details_of(want["repo"], rid, by_id[rid]["head_sha"], blobs),
            want,
        )
    except Exception as e:  # noqa: BLE001 - any failure means a full build
        detail = str(e).strip().replace("\n", " ")
        if isinstance(e, subprocess.CalledProcessError) and e.stderr:
            detail += " " + e.stderr.strip().replace("\n", " ")
        return none, [
            f"::warning::tag_reuse could not read the earlier runs ({type(e).__name__}: {detail}): building everything"
        ]


def main(argv, env):
    if len(argv) != 1:
        print("usage: tag_reuse.py (configured by the environment)", file=sys.stderr)
        return 2
    picks, reasons = check(env)
    for r in reasons:
        print(r)
    for kind in ORDER:
        print(f"reuse_{kind}={picks[kind] or ''}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv, os.environ))
