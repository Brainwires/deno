#!/usr/bin/env python3
"""The engine staleness gate of denext_runtime.yml: how far behind Chrome
stable are the JavaScript and browser engines this runtime ships?

The fork pins two Chromium-derived engines:

- V8, through the `v8` crate (rusty_v8) in Cargo.lock. The crate's major is
  the Chrome milestone of the V8 it carries: v8 152.2.0 is V8 15.2, which
  shipped in Chrome 152.
- CEF, through laufey's `CEF_FULL_VERSION` (its Makefile), such as
  `150.0.21+ga61e9a5+chromium-150.0.7871.255`: Chrome milestone 150.

V8 and Chromium patch security bugs on the current stable milestone and on
Chrome's Extended Stable milestone (an even milestone, kept for eight weeks
after the next one reaches stable) only. An engine more than one Chrome
stable major behind and not on the Extended Stable milestone receives no
fixes for public JIT and type-confusion bugs, which untrusted JavaScript can
reach. So a pin is STALE when its milestone is below `stable - 1` and below
Extended Stable.

A stale pin is a `::warning::` on every run, and an `::error::` that fails
the run at a release tag (`--tag`) when its engine's mode is `tag` (the
default); `warn` keeps a tag at a warning too. ENGINE_STALENESS_ENFORCE sets
the mode of both engines; ENGINE_STALENESS_ENFORCE_V8 and
ENGINE_STALENESS_ENFORCE_CEF override it for one (denext_runtime.yml enforces
both: V8 since the fork builds its own, DENEXT.md item 218, and CEF since
laufey moved to CEF 156). A pin behind stable but
on the Extended Stable milestone is not stale: it is a `::warning::` that
says it still gets fixes, on Extended Stable's schedule, and never fails a
tag. When Extended Stable cannot be learned, a pin is judged against stable
alone.

A supported milestone is not enough: Google merges security fixes onto
the milestone's branch as patch releases, and a pin that does not take them
keeps the bugs they fix (plans/fork-own-v8.md, "The engine staleness gate").
So a pin is also STALE when its patch level trails the newest patch on its
own branch by more than PATCH_LAG_DAYS (14) days:

- V8: the pin is the V8 version in the `v8` crate's own
  `v8/include/v8-version.h` (Cargo.lock's git source of Brainwires/rusty_v8,
  such as 15.6.75.33), its branch V8's `branch-heads/<major>.<minor>`, and
  each patch a `Version <x.y.z.w>` commit there (gitiles), dated by its
  committer time.
- CEF: the pin is CEF_FULL_VERSION, its branch every CEF build on the same
  Chromium milestone in CEF's build index (cef-builds.spotifycdn.com), each
  dated by its upload.

The plan's "no more than 14 days behind that branch head's newest Version
commit" is read strictly: a pin is patch-stale when EITHER the branch's
newest patch is more than 14 days newer than the pin, OR the oldest patch
the pin lacks has been out for more than 14 days (now - its date). The
first alone would pass a pin that skipped one early fix forever; the second
alone would pass a pin far behind a branch that only just moved. A
patch-stale pin is treated like a milestone-stale one: a `::warning::` on a
branch, an `::error::` at a tag in `tag` mode.

Offline-safe: when Chrome stable cannot be learned (no network, an HTTP
error, an answer it cannot read) the gate warns and passes the milestone
check, whatever the mode; when a pin's patch level or its branch cannot be
learned, it warns and passes the patch check. A pin it cannot read from the
repository is a repository bug, and fails.

Usage: engine_staleness.py [--tag] [--cef <CEF_FULL_VERSION>]
                           [--lock <Cargo.lock>] [--stable <milestone>]
                           [--extended <milestone>] [--v8-version <x.y.z.w>]
                           [--now <ISO 8601 time>]
Env:   ENGINE_STALENESS_ENFORCE  `tag` (default) or `warn`
       ENGINE_STALENESS_ENFORCE_V8, ENGINE_STALENESS_ENFORCE_CEF
                                 the same, for one engine (default: the above)
       CHROME_STABLE_URL         the versionhistory endpoints (tests point
       CHROME_EXTENDED_URL       them at a file:// answer or a closed port)
       V8_PIN_HEADER_URL         the patch-level sources, as templates
       V8_BRANCH_LOG_URL         ({repo} {rev}, {branch}, {version}); tests
       V8_TAG_URL                point them at file:// answers or a closed
       CEF_INDEX_URL             port
Prints one line per engine and check and exits 0, or 1 when a tag is
refused.
"""

import argparse
import datetime
import json
import os
import re
import sys
import urllib.request

CHROME_STABLE_URL = (
    "https://versionhistory.googleapis.com/v1/chrome/platforms/linux"
    "/channels/stable/versions?pageSize=5"
)
# Extended Stable is a Windows and macOS channel (Linux has none).
CHROME_EXTENDED_URL = (
    "https://versionhistory.googleapis.com/v1/chrome/platforms/win"
    "/channels/extended/versions?pageSize=5"
)
# How many Chrome stable majors a pin may trail before it is stale.
ALLOWED_LAG = 1
# How many days a pin's patch level may trail its branch (see above).
PATCH_LAG_DAYS = 14
TIMEOUT_SECONDS = 15

# The V8 inside the pinned `v8` crate: the crate source keeps V8's headers.
V8_PIN_HEADER_URL = "https://raw.githubusercontent.com/{repo}/{rev}/v8/include/v8-version.h"
# V8's milestone branch, newest commit first. 200 commits is months of
# merges on a release branch (each merge is two commits: the fix and its
# `Version` commit).
V8_BRANCH_LOG_URL = (
    "https://chromium.googlesource.com/v8/v8/+log/refs/branch-heads/{branch}"
    "?format=JSON&n=200"
)
# One V8 version's tag, for a pin older than the log above reaches.
V8_TAG_URL = "https://chromium.googlesource.com/v8/v8/+/refs/tags/{version}?format=JSON"
# CEF's build index (every published build, per platform; about 10 MB).
CEF_INDEX_URL = "https://cef-builds.spotifycdn.com/index.json"
# The index platform the CEF dates are read from (CEF uploads every platform
# of a build together).
CEF_INDEX_PLATFORM = "linux64"


def v8_crate_version(lock_text):
    """The version of the `v8` package in a Cargo.lock, or None."""
    for block in lock_text.split("[[package]]"):
        if re.search(r'^name = "v8"$', block, re.M):
            m = re.search(r'^version = "([^"]+)"$', block, re.M)
            if m:
                return m.group(1)
    return None


def v8_milestone(crate_version):
    """(milestone, "major.minor") of the V8 inside rusty_v8 `crate_version`:
    the crate's major is the Chrome milestone, 152 -> V8 15.2."""
    m = re.match(r"^(\d+)\.\d+\.\d+", crate_version or "")
    if not m:
        return None
    milestone = int(m.group(1))
    major, minor = divmod(milestone, 10)
    return milestone, f"{major}.{minor}"


def cef_milestone(full_version):
    """The Chrome milestone of a CEF_FULL_VERSION: the `chromium-<M>.` part,
    else CEF's own leading major (CEF numbers itself after Chromium)."""
    if not full_version:
        return None
    m = re.search(r"chromium-(\d+)\.", full_version)
    if not m:
        m = re.match(r"^(\d+)\.", full_version.strip())
    return int(m.group(1)) if m else None


def stable_from_answer(body):
    """The newest Chrome stable milestone in a versionhistory answer."""
    versions = json.loads(body).get("versions") or []
    majors = [
        int(v["version"].split(".", 1)[0])
        for v in versions
        if re.match(r"^\d+\.", v.get("version", ""))
    ]
    if not majors:
        raise ValueError("no stable versions in the answer")
    return max(majors)


def fetch_text(url):
    """The body of `url` (http(s) or file://), as text. Raises on failure."""
    req = urllib.request.Request(url, headers={"User-Agent": "denext-runtime-ci"})
    with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as res:
        return res.read().decode("utf-8")


def fetch_stable(url):
    """(milestone, None), or (None, reason) when it cannot be learned."""
    try:
        return stable_from_answer(fetch_text(url)), None
    except Exception as e:  # noqa: BLE001 - every failure is "offline"
        return None, f"{type(e).__name__}: {e}"


def v8_crate_source(lock_text):
    """(repo, rev) of the `v8` package's GitHub git source in a Cargo.lock
    (`git+https://github.com/<owner>/<repo>?rev=...#<sha>`), else None (a
    registry crate, whose V8 headers the gate does not fetch)."""
    for block in lock_text.split("[[package]]"):
        if re.search(r'^name = "v8"$', block, re.M):
            m = re.search(
                r'^source = "git\+https://github\.com/([^/?#"]+/[^/?#"]+?)(?:\.git)?'
                r'(?:\?[^#"]*)?#([0-9a-f]{40})"$',
                block,
                re.M,
            )
            return (m.group(1), m.group(2)) if m else None
    return None


def v8_version_from_header(text):
    """"15.6.75.33" from a v8-version.h, or None."""
    parts = []
    for macro in ("MAJOR_VERSION", "MINOR_VERSION", "BUILD_NUMBER", "PATCH_LEVEL"):
        m = re.search(rf"^#define V8_{macro} (\d+)\s*$", text, re.M)
        if not m:
            return None
        parts.append(m.group(1))
    return ".".join(parts)


def gitiles_json(text):
    """A gitiles JSON answer, without its `)]}'` XSSI guard line."""
    if text.startswith(")]}'"):
        text = text.split("\n", 1)[1] if "\n" in text else ""
    return json.loads(text)


def gitiles_time(value):
    """A gitiles commit time ("Tue Nov 03 12:37:55 2026", UTC, sometimes
    with a " +0000" offset) as an aware UTC datetime."""
    m = re.match(r"^(\w{3} \w{3} +\d{1,2} \d{2}:\d{2}:\d{2} \d{4})(?: ([+-]\d{4}))?$", value.strip())
    if not m:
        raise ValueError(f"unreadable gitiles time {value!r}")
    t = datetime.datetime.strptime(re.sub(" +", " ", m.group(1)), "%a %b %d %H:%M:%S %Y")
    offset = m.group(2) or "+0000"
    sign = 1 if offset[0] == "+" else -1
    delta = datetime.timedelta(hours=int(offset[1:3]), minutes=int(offset[3:5]))
    return (t - sign * delta).replace(tzinfo=datetime.timezone.utc)


def v8_branch_versions(log_text):
    """[(version, datetime)] of the `Version x.y.z.w` commits in a gitiles
    +log answer, newest first."""
    out = []
    for commit in gitiles_json(log_text).get("log") or []:
        subject = (commit.get("message") or "").split("\n", 1)[0].strip()
        m = re.match(r"^Version (\d+\.\d+\.\d+\.\d+)$", subject)
        if m:
            out.append((m.group(1), gitiles_time(commit["committer"]["time"])))
    return out


def iso_time(value):
    """An ISO 8601 time ("2026-10-31T05:23:06.373000Z") as an aware UTC
    datetime."""
    t = datetime.datetime.fromisoformat(value.strip().replace("Z", "+00:00"))
    if t.tzinfo is None:
        t = t.replace(tzinfo=datetime.timezone.utc)
    return t.astimezone(datetime.timezone.utc)


def cef_branch_versions(index_text, pin, platform=CEF_INDEX_PLATFORM):
    """[(cef_version, datetime)] of every CEF build in CEF's build index on
    the Chromium milestone of `pin`, newest first; each is dated by its
    earliest file upload."""
    milestone = cef_milestone(pin)
    builds = (json.loads(index_text).get(platform) or {}).get("versions") or []
    out = []
    for build in builds:
        version = build.get("cef_version") or ""
        if cef_milestone(version) != milestone:
            continue
        dates = [iso_time(f["last_modified"]) for f in build.get("files") or [] if f.get("last_modified")]
        if dates:
            out.append((version, min(dates)))
    out.sort(key=lambda v: v[1], reverse=True)
    return out


def patch_lag(pin, pin_date, versions, now):
    """How far `pin` (released `pin_date`) trails its branch `versions`
    [(version, datetime)] at `now`: None when no newer patch exists, else
    {"newest", "behind_days", "unpatched_days", "lag_days"}. behind_days is
    the newest patch's date minus the pin's; unpatched_days is how long the
    oldest patch the pin lacks has been out; lag_days the larger of the two
    (the strict reading of the plan's rule)."""
    newer = [(v, d) for v, d in versions if v != pin and d > pin_date]
    if not newer:
        return None
    newest = max(newer, key=lambda v: v[1])
    oldest = min(newer, key=lambda v: v[1])
    behind = (newest[1] - pin_date).total_seconds() / 86400
    unpatched = (now - oldest[1]).total_seconds() / 86400
    return {
        "newest": newest[0],
        "behind_days": behind,
        "unpatched_days": unpatched,
        "lag_days": max(behind, unpatched),
    }


def days(value):
    """"1 day", "3 days" (whole days, rounded down)."""
    n = int(value)
    return f"{n} day" if n == 1 else f"{n} days"


def patch_verdict(name, pin, lag):
    """(state, line) of one engine's patch check: "current" or "stale"."""
    if lag is None:
        return "current", f"{name} is on its branch's newest patch"
    detail = (
        f"its branch's newest patch {lag['newest']} is {days(lag['behind_days'])} "
        f"newer, and the first patch it lacks has been out {days(lag['unpatched_days'])}"
    )
    if lag["lag_days"] <= PATCH_LAG_DAYS:
        return "current", (
            f"{name} trails its branch's patches by at most {PATCH_LAG_DAYS} days ({detail})"
        )
    return "stale", (
        f"{name} is more than {PATCH_LAG_DAYS} days behind its branch's patches "
        f"({detail}; at most {PATCH_LAG_DAYS} days allowed): it lacks upstream "
        f"security fixes its branch already has"
    )


def v8_patch_check(name, lock_text, pin_version, now):
    """(state, line) of the V8 pin's patch check: "current", "stale" or
    "unknown" (could not learn; warns and passes)."""
    try:
        if pin_version is None:
            source = v8_crate_source(lock_text)
            if source is None:
                return "unknown", (
                    f"could not learn the patch level of {name}: its Cargo.lock source "
                    f"is not a GitHub git revision; patch staleness not checked"
                )
            url = (os.environ.get("V8_PIN_HEADER_URL") or V8_PIN_HEADER_URL).format(
                repo=source[0], rev=source[1]
            )
            pin_version = v8_version_from_header(fetch_text(url))
            if pin_version is None:
                raise ValueError(f"no V8 version macros in {url}")
        branch = ".".join(pin_version.split(".")[:2])
        log_url = (os.environ.get("V8_BRANCH_LOG_URL") or V8_BRANCH_LOG_URL).format(branch=branch)
        versions = v8_branch_versions(fetch_text(log_url))
        if not versions:
            raise ValueError(f"no Version commits on V8's branch-heads/{branch}")
        pin_date = dict(versions).get(pin_version)
        if pin_date is None:
            tag_url = (os.environ.get("V8_TAG_URL") or V8_TAG_URL).format(version=pin_version)
            pin_date = gitiles_time(gitiles_json(fetch_text(tag_url))["committer"]["time"])
    except Exception as e:  # noqa: BLE001 - every failure is "offline"
        return "unknown", (
            f"could not learn the patch level of {name} ({type(e).__name__}: {e}); "
            f"patch staleness not checked"
        )
    return patch_verdict(f"V8 {pin_version}", pin_version, patch_lag(pin_version, pin_date, versions, now))


def cef_patch_check(name, pin, now):
    """(state, line) of the CEF pin's patch check, as v8_patch_check."""
    try:
        versions = cef_branch_versions(fetch_text(os.environ.get("CEF_INDEX_URL") or CEF_INDEX_URL), pin)
        pin_date = dict(versions).get(pin)
        if pin_date is None:
            raise ValueError(f"{pin} is not in CEF's build index")
    except Exception as e:  # noqa: BLE001 - every failure is "offline"
        return "unknown", (
            f"could not learn the patch level of {name} ({type(e).__name__}: {e}); "
            f"patch staleness not checked"
        )
    return patch_verdict(name, pin, patch_lag(pin, pin_date, versions, now))


def verdict(name, milestone, stable, extended=None):
    """(state, line) for one engine pinned at Chrome `milestone`: state is
    "current", "extended" (behind stable, on the Extended Stable milestone
    `extended`) or "stale". `extended` None: not known."""
    behind = stable - milestone
    if behind <= ALLOWED_LAG:
        return "current", f"{name} is current enough (milestone {milestone}, stable {stable})"
    if extended is not None and milestone >= extended:
        return "extended", (
            f"{name} is {behind} Chrome stable majors behind (milestone "
            f"{milestone}, stable {stable}) but on Chrome's Extended Stable "
            f"milestone {extended}: it still gets upstream security fixes, on "
            f"Extended Stable's schedule rather than Stable's"
        )
    on_extended = f", Extended Stable {extended}" if extended is not None else ""
    return "stale", (
        f"{name} is {behind} Chrome stable majors behind (milestone "
        f"{milestone}, stable {stable}{on_extended}; at most {ALLOWED_LAG} "
        f"allowed): it no longer receives upstream security fixes"
    )


def evaluate(engines, stable, tag, enforce, extended=None):
    """(exit code, lines) for `engines` [(name, milestone)] against Chrome
    stable `stable` (None: not known, the milestone check is skipped) and
    Extended Stable `extended` (None: not known). An engine given as (name,
    milestone, mode) uses that mode instead of `enforce`; one given as
    (name, milestone, mode, patch) also reports `patch`, its patch check's
    (state, line) ("current", "stale" or "unknown")."""
    lines = []
    refused = False
    for engine in engines:
        name, milestone = engine[0], engine[1]
        mode = engine[2] if len(engine) > 2 and engine[2] else enforce
        checks = []
        if stable is not None:
            checks.append(verdict(name, milestone, stable, extended))
        if len(engine) > 3 and engine[3] is not None:
            checks.append(engine[3])
        for state, line in checks:
            if state == "current":
                lines.append(line)
            elif state in ("extended", "unknown"):
                lines.append(f"::warning::{line}")
            elif tag and mode == "tag":
                lines.append(f"::error::{line}")
                refused = True
            else:
                lines.append(f"::warning::{line}")
    return (1 if refused else 0), lines


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("--tag", action="store_true", help="the run builds a release tag")
    p.add_argument("--cef", help="laufey's CEF_FULL_VERSION")
    p.add_argument("--lock", default="Cargo.lock")
    p.add_argument("--stable", type=int, help="Chrome stable milestone (skips the lookup)")
    p.add_argument(
        "--extended", type=int, help="Chrome Extended Stable milestone (skips its lookup)"
    )
    p.add_argument(
        "--v8-version", help="the pinned V8 version, x.y.z.w (skips reading the crate's header)"
    )
    p.add_argument("--now", help="the time the patch lag is measured at (ISO 8601; default: now)")
    args = p.parse_args(argv)
    now = iso_time(args.now) if args.now else datetime.datetime.now(datetime.timezone.utc)

    enforce = os.environ.get("ENGINE_STALENESS_ENFORCE", "tag") or "tag"
    modes = {}
    for var, value in (
        ("ENGINE_STALENESS_ENFORCE", enforce),
        ("ENGINE_STALENESS_ENFORCE_V8", os.environ.get("ENGINE_STALENESS_ENFORCE_V8") or enforce),
        ("ENGINE_STALENESS_ENFORCE_CEF", os.environ.get("ENGINE_STALENESS_ENFORCE_CEF") or enforce),
    ):
        if value not in ("tag", "warn"):
            print(f"::error::{var} is '{value}', want 'tag' or 'warn'")
            return 1
        modes[var] = value

    if args.v8_version is not None and not re.match(r"^\d+\.\d+\.\d+\.\d+$", args.v8_version):
        print(f"::error::--v8-version is '{args.v8_version}', want x.y.z.w")
        return 1

    with open(args.lock, encoding="utf-8") as f:
        lock_text = f.read()
    crate = v8_crate_version(lock_text)
    v8 = v8_milestone(crate)
    if v8 is None:
        print(f"::error::no readable v8 package version in {args.lock} (got {crate!r})")
        return 1
    v8_name = f"V8 {v8[1]} (v8 crate {crate})"
    engines = [
        (
            v8_name,
            v8[0],
            modes["ENGINE_STALENESS_ENFORCE_V8"],
            v8_patch_check(v8_name, lock_text, args.v8_version, now),
        )
    ]
    if args.cef is not None:
        cef = cef_milestone(args.cef)
        if cef is None:
            print(f"::error::cannot read a Chrome milestone from CEF_FULL_VERSION '{args.cef}'")
            return 1
        cef_name = f"CEF {args.cef}"
        engines.append(
            (cef_name, cef, modes["ENGINE_STALENESS_ENFORCE_CEF"], cef_patch_check(cef_name, args.cef, now))
        )

    stable = args.stable
    if stable is None:
        stable, reason = fetch_stable(os.environ.get("CHROME_STABLE_URL") or CHROME_STABLE_URL)
        if stable is None:
            # The patch checks below still run.
            print(
                f"::warning::could not learn the Chrome stable milestone ({reason}); "
                f"milestone staleness not checked: {', '.join(e[0] for e in engines)}"
            )

    extended = args.extended
    # Only a pin behind stable needs Extended Stable.
    if extended is None and stable is not None and any(stable - e[1] > ALLOWED_LAG for e in engines):
        extended, reason = fetch_stable(
            os.environ.get("CHROME_EXTENDED_URL") or CHROME_EXTENDED_URL
        )
        if extended is None:
            print(
                f"::notice::could not learn the Chrome Extended Stable milestone "
                f"({reason}); engines judged against stable alone"
            )

    code, lines = evaluate(engines, stable, args.tag, enforce, extended)
    for line in lines:
        print(line)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
