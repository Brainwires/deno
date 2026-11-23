#!/usr/bin/env python3
"""Fails when an advisory .cargo/audit.toml ignores has become reachable.

Some ignores in .cargo/audit.toml hold only while Cargo.lock stays below a
version: RUSTSEC-2026-0418 (libz-sys's build script prints the compiler
command) is unreachable only because cc 1.6.0 and later, which put the whole
build environment on that command, are not in the lockfile. A `cargo update`
that brings such a version in would keep `cargo audit` green while the
advisory applied. This check runs beside `cargo audit` (denext_tests.yml's
cargo-audit job) and fails then, naming the advisory and the crate.

Others hold only while a crate comes from a vendored copy that carries the
fix: RUSTSEC-2026-0592 (url's `Url::origin()` overflows the stack on nested
`blob:` URLs) is ignored because `url` is libs/vendor/url, 2.5.7 with the fix
backported (url 2.5.9 percent-encodes `^` in paths, which breaks `npm:` and
`jsr:` specifiers). cargo audit matches on the version alone, so it still
reports that copy. The ignore fails here once `url` comes from a registry
again or the vendored file no longer carries the backport.

Usage: audit_ignores.py [--audit-toml <path>] [--lock <path>] [--root <dir>]
Exits 0, or 1 with one `::error::` line per reachable ignore.
"""

import argparse
import os
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

# advisory -> (crate, vendored file that must carry the fix, marker in it, why)
VENDORED = {
    "RUSTSEC-2026-0592": (
        "url",
        "libs/vendor/url/src/origin.rs",
        "RUSTSEC-2026-0592",
        "the ignore holds only while url is the vendored 2.5.7 with the "
        "nested-blob: origin fix backported",
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


def lock_sources(lock_text, crate):
    """The `source` of every `crate` package in a Cargo.lock (None for a
    path dependency, which has no source line)."""
    sources = []
    for block in lock_text.split("[[package]]"):
        if re.search(rf'^name = "{re.escape(crate)}"$', block, re.M):
            m = re.search(r'^source = "([^"]+)"$', block, re.M)
            sources.append(m.group(1) if m else None)
    return sources


def check_vendored(audit_toml, lock_text, read_file):
    """The `::error::` lines for every ignored advisory whose vendored fix
    is no longer what the build uses. read_file(path) returns the file's
    text, or None when it is missing."""
    errors = []
    ignored = ignored_advisories(audit_toml)
    for advisory, (crate, path, marker, why) in sorted(VENDORED.items()):
        if advisory not in ignored:
            continue
        sources = lock_sources(lock_text, crate)
        registry = [s for s in sources if s is not None]
        if not sources or registry:
            errors.append(
                f"::error::.cargo/audit.toml ignores {advisory}, but Cargo.lock's "
                f"{crate} is not (only) the vendored path copy "
                f"({', '.join(registry) or 'no such package'}): {why}. "
                f"Take a fixed {crate} and drop the ignore."
            )
            continue
        text = read_file(path)
        if text is None or marker not in text:
            errors.append(
                f"::error::.cargo/audit.toml ignores {advisory}, but {path} "
                f"{'is missing' if text is None else 'no longer names ' + marker}: {why}."
            )
    return errors


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("--audit-toml", default=".cargo/audit.toml")
    p.add_argument("--lock", default="Cargo.lock")
    p.add_argument("--root", default=".")
    args = p.parse_args(argv)
    with open(args.audit_toml, encoding="utf-8") as f:
        audit_toml = f.read()
    with open(args.lock, encoding="utf-8") as f:
        lock_text = f.read()
    def read_file(path):
        try:
            with open(os.path.join(args.root, path), encoding="utf-8") as f:
                return f.read()
        except FileNotFoundError:
            return None

    errors = check(audit_toml, lock_text) + check_vendored(audit_toml, lock_text, read_file)
    for line in errors:
        print(line)
    if not errors:
        held = sorted(ignored_advisories(audit_toml) & (set(CONDITIONS) | set(VENDORED)))
        print(f"conditional audit ignores still unreachable: {', '.join(held) or 'none'}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
