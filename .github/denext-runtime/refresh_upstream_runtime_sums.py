#!/usr/bin/env python3
"""Regenerate cli/upstream_runtime_sums.lock: the SHA-256 of every runtime
archive this fork's CLI can download from dl.deno.land (DENEXT.md item 215).

Hashes the bytes of each archive on dl.deno.land, the host the CLI downloads
from, and requires them to match the `<archive>.sha256sum` beside it. Compares
upstream's GitHub release where it has the archive and prints the result; review
that output before committing. Writes the lock file only when all of them pass.

Usage: refresh_upstream_runtime_sums.py <upstream version> <output file>
"""
import hashlib
import re
import sys
import urllib.error
import urllib.request

TARGETS = (
    "aarch64-apple-darwin",
    "aarch64-pc-windows-msvc",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
)
PREFIXES = ("denort", "denort-quickjs", "libdenort", "libdenort-quickjs")
DL = "https://dl.deno.land/release/v{version}/{name}"
GITHUB = "https://github.com/denoland/deno/releases/download/v{version}/{name}"
HEADER = """\
# Pinned SHA-256 digests of upstream Deno's runtime archives.
#
# The trust anchor for what this fork's CLI downloads from dl.deno.land:
# `deno desktop`'s libdenort, `--engine quickjs`, and `denort` in a build
# that is not denext's (cli/standalone/binary.rs, DENEXT.md item 215). An
# archive with other bytes is refused. cli/build.rs fails the build when the
# `# version:` below is not cli/lib/upstream_version.txt.
#
# Regenerate when the upstream version changes:
#   python3 -I .github/denext-runtime/refresh_upstream_runtime_sums.py \\
#     <version> cli/upstream_runtime_sums.lock
#
# Format: GNU `sha256sum` style, one `<hex>  <filename>` per line.
#
"""


def get(url, timeout):
    # dl.deno.land answers 403 to urllib's default User-Agent.
    request = urllib.request.Request(url, headers={"User-Agent": "denext-refresh-sums"})
    return urllib.request.urlopen(request, timeout=timeout)


def sum_in(text):
    """The SHA-256 in a `.sha256sum` file: `<hex>  <name>` from sha256sum, or
    PowerShell's `Get-FileHash` table on Windows."""
    match = re.search(r"\b[0-9a-fA-F]{64}\b", text)
    return match.group(0).lower() if match else None


def fetch_sum(url):
    try:
        with get(url, 60) as response:
            return sum_in(response.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as err:
        if err.code == 404:
            return None
        raise


def hash_of(url):
    digest = hashlib.sha256()
    with get(url, 120) as response:
        while chunk := response.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def render(version, sums):
    lines = [f"{sums[name]}  {name}" for name in sorted(sums)]
    return f"{HEADER}# version: v{version}\n" + "\n".join(lines) + "\n"


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    version, output = sys.argv[1], sys.argv[2]
    sums = {}
    for prefix in PREFIXES:
        for target in TARGETS:
            name = f"{prefix}-{target}.zip"
            actual = hash_of(DL.format(version=version, name=name))
            listed = fetch_sum(DL.format(version=version, name=f"{name}.sha256sum"))
            if listed != actual:
                raise SystemExit(f"{name}: bytes {actual}, .sha256sum {listed}")
            mirror = fetch_sum(GITHUB.format(version=version, name=f"{name}.sha256sum"))
            # Reported, not fatal: upstream's two hosts can hold separate builds
            # of one release (v2.9.8's denort differs, built 85 minutes apart).
            github = "absent" if mirror is None else "match" if mirror == actual else f"DIFFERS {mirror}"
            print(f"{actual}  {name}  github={github}", flush=True)
            sums[name] = actual
    with open(output, "w", newline="\n") as f:
        f.write(render(version, sums))


if __name__ == "__main__":
    main()
