#!/usr/bin/env python3
"""The engine staleness gate of denext_runtime.yml: how far behind Chrome
stable are the JavaScript and browser engines this runtime ships?

The fork pins two Chromium-derived engines:

- V8, through the `v8` crate (rusty_v8) in Cargo.lock. The crate's major is
  the Chrome milestone of the V8 it carries: v8 152.2.0 is V8 15.2, which
  shipped in Chrome 152.
- CEF, through laufey's `CEF_FULL_VERSION` (its Makefile), such as
  `150.0.21+ga61e9a5+chromium-150.0.7871.255`: Chrome milestone 150.

V8 and Chromium patch security bugs on the current stable milestone (and the
extended-stable one before it) only. An engine more than one Chrome stable
major behind receives no fixes for public JIT and type-confusion bugs, which
untrusted JavaScript can reach. So a pin is STALE when its milestone is below
`stable - 1`.

A stale pin is a `::warning::` on every run, and an `::error::` that fails
the run at a release tag (`--tag`) when ENGINE_STALENESS_ENFORCE is `tag`
(the default); `warn` keeps a tag at a warning too.

Offline-safe: when Chrome stable cannot be learned (no network, an HTTP
error, an answer it cannot read) the gate warns and passes, whatever the
mode. A pin it cannot read is a repository bug, and fails.

Usage: engine_staleness.py [--tag] [--cef <CEF_FULL_VERSION>]
                           [--lock <Cargo.lock>] [--stable <milestone>]
Env:   ENGINE_STALENESS_ENFORCE  `tag` (default) or `warn`
       CHROME_STABLE_URL         the versionhistory endpoint (tests point it
                                 at a file:// answer or a closed port)
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


def verdict(name, milestone, stable):
    """(stale, line) for one engine pinned at Chrome `milestone`."""
    behind = stable - milestone
    if behind > ALLOWED_LAG:
        return True, (
            f"{name} is {behind} Chrome stable majors behind (milestone "
            f"{milestone}, stable {stable}; at most {ALLOWED_LAG} allowed): "
            f"it no longer receives upstream security fixes"
        )
    return False, f"{name} is current enough (milestone {milestone}, stable {stable})"


def evaluate(engines, stable, tag, enforce):
    """(exit code, lines) for `engines` [(name, milestone)] against Chrome
    stable `stable`."""
    lines = []
    refused = False
    for name, milestone in engines:
        stale, line = verdict(name, milestone, stable)
        if not stale:
            lines.append(line)
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

    code, lines = evaluate(engines, stable, args.tag, enforce)
    for line in lines:
        print(line)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
