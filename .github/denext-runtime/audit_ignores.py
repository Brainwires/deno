#!/usr/bin/env python3
"""Fails when an advisory .cargo/audit.toml ignores has become reachable.

Some ignores in .cargo/audit.toml hold only while Cargo.lock stays below a
version: RUSTSEC-2026-0418 (libz-sys's build script prints the compiler
command) is unreachable only because cc 1.6.0 and later, which put the whole
build environment on that command, are not in the lockfile. A `cargo update`
that brings such a version in would keep `cargo audit` green while the
advisory applied. This check runs beside `cargo audit` (denext_tests.yml's
cargo-audit job) and fails then, naming the advisory and the crate.

Usage: audit_ignores.py [--audit-toml <path>] [--lock <path>]
Exits 0, or 1 with one `::error::` line per reachable ignore.
"""

import argparse
import re
import sys

# advisory -> (crate, first version that makes it reachable, why)
CONDITIONS = {
    "RUSTSEC-2026-0418": (
        "cc",
        (1, 6, 0),
        "cc 1.6.0 and later put the build environment on the compiler "
        "command libz-sys's build script prints",
    ),
}


def ignored_advisories(audit_toml):
    """The advisory ids in audit.toml's `[advisories] ignore = [...]`,
    comments left out."""
    text = "\n".join(line.split("#", 1)[0] for line in audit_toml.splitlines())
    m = re.search(r"^\s*ignore\s*=\s*\[(.*?)\]", text, re.S | re.M)
    if not m:
        return set()
    return set(re.findall(r'"([A-Z]+-\d{4}-\d{4})"', m.group(1)))


def lock_versions(lock_text, crate):
    """Every version of `crate` in a Cargo.lock."""
    versions = []
    for block in lock_text.split("[[package]]"):
        if re.search(rf'^name = "{re.escape(crate)}"$', block, re.M):
            m = re.search(r'^version = "([^"]+)"$', block, re.M)
            if m:
                versions.append(m.group(1))
    return versions


def version_key(version):
    """(major, minor, patch) of a semver string; pre-release and build
    metadata left out."""
    core = re.split(r"[-+]", version, maxsplit=1)[0]
    parts = [int(p) for p in core.split(".")[:3] if p.isdigit()]
    return tuple(parts + [0] * (3 - len(parts)))


def check(audit_toml, lock_text):
    """The `::error::` lines for every ignored advisory that has become
    reachable."""
    errors = []
    ignored = ignored_advisories(audit_toml)
    for advisory, (crate, first, why) in sorted(CONDITIONS.items()):
        if advisory not in ignored:
            continue
        bad = [v for v in lock_versions(lock_text, crate) if version_key(v) >= first]
        if bad:
            errors.append(
                f"::error::.cargo/audit.toml ignores {advisory}, but Cargo.lock has "
                f"{crate} {', '.join(bad)} (>= {'.'.join(map(str, first))}): {why}. "
                f"Fix the advisory (or pin {crate} lower) and drop the ignore."
            )
    return errors


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("--audit-toml", default=".cargo/audit.toml")
    p.add_argument("--lock", default="Cargo.lock")
    args = p.parse_args(argv)
    with open(args.audit_toml, encoding="utf-8") as f:
        audit_toml = f.read()
    with open(args.lock, encoding="utf-8") as f:
        lock_text = f.read()
    errors = check(audit_toml, lock_text)
    for line in errors:
        print(line)
    if not errors:
        held = sorted(ignored_advisories(audit_toml) & set(CONDITIONS))
        print(f"conditional audit ignores still unreachable: {', '.join(held) or 'none'}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
