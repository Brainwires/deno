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
the run at a release tag (`--tag`) when ENGINE_STALENESS_ENFORCE is `tag`
(the default); `warn` keeps a tag at a warning too. A pin behind stable but
on the Extended Stable milestone is not stale: it is a `::warning::` that
says it still gets fixes, on Extended Stable's schedule, and never fails a
tag. When Extended Stable cannot be learned, a pin is judged against stable
alone.

Offline-safe: when Chrome stable cannot be learned (no network, an HTTP
error, an answer it cannot read) the gate warns and passes, whatever the
mode. A pin it cannot read is a repository bug, and fails.

Usage: engine_staleness.py [--tag] [--cef <CEF_FULL_VERSION>]
                           [--lock <Cargo.lock>] [--stable <milestone>]
                           [--extended <milestone>]
Env:   ENGINE_STALENESS_ENFORCE  `tag` (default) or `warn`
       CHROME_STABLE_URL         the versionhistory endpoints (tests point
       CHROME_EXTENDED_URL       them at a file:// answer or a closed port)
Prints one line per engine and exits 0, or 1 when a tag is refused.
"""

import argparse
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
TIMEOUT_SECONDS = 15


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


def fetch_stable(url):
    """(milestone, None), or (None, reason) when it cannot be learned."""
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "denext-runtime-ci"})
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as res:
            return stable_from_answer(res.read().decode("utf-8")), None
    except Exception as e:  # noqa: BLE001 - every failure is "offline"
        return None, f"{type(e).__name__}: {e}"


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
    stable `stable` and Extended Stable `extended` (None: not known)."""
    lines = []
    refused = False
    for name, milestone in engines:
        state, line = verdict(name, milestone, stable, extended)
        if state == "current":
            lines.append(line)
        elif state == "extended":
            lines.append(f"::warning::{line}")
        elif tag and enforce == "tag":
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
    args = p.parse_args(argv)

    enforce = os.environ.get("ENGINE_STALENESS_ENFORCE", "tag") or "tag"
    if enforce not in ("tag", "warn"):
        print(f"::error::ENGINE_STALENESS_ENFORCE is '{enforce}', want 'tag' or 'warn'")
        return 1

    with open(args.lock, encoding="utf-8") as f:
        crate = v8_crate_version(f.read())
    v8 = v8_milestone(crate)
    if v8 is None:
        print(f"::error::no readable v8 package version in {args.lock} (got {crate!r})")
        return 1
    engines = [(f"V8 {v8[1]} (v8 crate {crate})", v8[0])]
    if args.cef is not None:
        cef = cef_milestone(args.cef)
        if cef is None:
            print(f"::error::cannot read a Chrome milestone from CEF_FULL_VERSION '{args.cef}'")
            return 1
        engines.append((f"CEF {args.cef}", cef))

    stable = args.stable
    if stable is None:
        stable, reason = fetch_stable(os.environ.get("CHROME_STABLE_URL") or CHROME_STABLE_URL)
        if stable is None:
            print(
                f"::warning::could not learn the Chrome stable milestone ({reason}); "
                f"engine staleness not checked: {', '.join(n for n, _ in engines)}"
            )
            return 0

    extended = args.extended
    # Only a pin behind stable needs Extended Stable.
    if extended is None and any(stable - m > ALLOWED_LAG for _, m in engines):
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
