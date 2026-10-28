#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Rejects a bare assertion in the release and test scripts (DENEXT.md item
141, the production audit's P6).

A shell statement that is only a check (`test ...`, `[ ... ]`, `[[ ... ]]`,
`grep -q ...`) stops a `set -e` script with no word on what failed: the job
log ends at the step, and whoever reads it has to rerun the script to learn
which check it was. Every such check names what it checked: through `need`
(lib.sh), or `|| { echo "::error::..." >&2; exit 1; }`. A check that is a
condition (`if`, `while`, `... && action`) is not an assertion and passes.

Checked: every `.sh` under .github/denext-runtime/ and the `run:` blocks of
.github/workflows/denext_*.yml.

Usage: assert_guard.py [file...]   (no files: the repository's own)
Exits 1 and prints each bare assertion as `file:line: statement`.
"""

import glob
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))

# The start of a statement that is only a check.
CHECK = re.compile(r"^(?:!\s+)?(?:test\s|\[\s|\[\[\s|grep\s+(?:-\w*\s+)*-\w*q)")
# What names a failure in an `||` branch.
MESSAGE = re.compile(r"\b(?:echo|printf)\b")
# An `||` branch that fails the script.
SILENT_FAIL = re.compile(r"\b(?:exit|return|false)\b")
# A heredoc opener (not a here-string, `<<<`): its delimiter.
HEREDOC = re.compile(r"(?<!<)<<-?\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")
# A line that goes on on the next one.
CONTINUES = re.compile(r"(?:\\|\|\||&&|\|)\s*$")


def statements(lines):
    """(first line number, statement) of each logical shell statement in
    `lines` (numbered from 1): continuation lines joined, comments, blank
    lines and heredoc bodies left out."""
    out = []
    current, start, heredoc = None, 0, None
    for number, raw in enumerate(lines, 1):
        line = raw.strip()
        if heredoc:
            if line == heredoc:
                heredoc = None
            continue
        if current is None:
            if not line or line.startswith("#"):
                continue
            current, start = line, number
        else:
            current = current.rstrip("\\").rstrip() + " " + line
        opener = HEREDOC.search(line)
        if opener:
            heredoc = opener.group(2)
        # A check's `|| { ...; }` group, over several lines, is part of it.
        open_group = CHECK.match(current) and current.count("{") > current.count("}")
        if not CONTINUES.search(line) and not open_group:
            out.append((start, current))
            current = None
    if current is not None:
        out.append((start, current))
    return out


def bare(statement):
    """Whether `statement` is an assertion that fails with no message."""
    if not CHECK.match(statement):
        return False
    if "||" in statement:
        # `check || { echo "::error::..."; exit 1; }` names it; `check ||
        # action` (`|| continue`, `|| args+=(...)`) is a condition. Only an
        # `|| exit` / `|| return` / `|| false` with no message is bare.
        tail = statement.split("||", 1)[1]
        if MESSAGE.search(tail):
            return False
        return bool(SILENT_FAIL.search(tail))
    # `check && action` is a condition; `check && check` is still an
    # assertion (set -e fails on the last one).
    parts = [p.strip() for p in statement.split("&&")]
    return all(CHECK.match(p) for p in parts)


def run_blocks(text):
    """(line number, lines) of each `run:` block of a workflow: a block scalar
    (`|`, `>`, with chomping) as its lines, a folded one as one line, a plain
    one-line `run:` as itself."""
    lines = text.split("\n")
    out = []
    i = 0
    while i < len(lines):
        m = re.match(r"^(\s*)(?:-\s+)?run:\s*(.*)$", lines[i])
        if not m:
            i += 1
            continue
        indent, value = len(m.group(1)), m.group(2).strip()
        if value and value[0] not in "|>":
            out.append((i + 1, [value]))
            i += 1
            continue
        body, first = [], i + 2
        i += 1
        while i < len(lines) and (
            not lines[i].strip() or len(lines[i]) - len(lines[i].lstrip()) > indent
        ):
            body.append(lines[i])
            i += 1
        if value.startswith(">"):
            body = [" ".join(l.strip() for l in body if l.strip())]
        out.append((first, body))
    return out


def check_file(path):
    """`file:line: statement` for each bare assertion in `path`."""
    with open(path, encoding="utf-8") as f:
        text = f.read()
    shown = os.path.relpath(path, ROOT) if os.path.isabs(path) else path
    found = []
    if path.endswith((".yml", ".yaml")):
        for first, block in run_blocks(text):
            for number, statement in statements(block):
                if bare(statement):
                    found.append(f"{shown}:{first + number - 1}: {statement}")
    else:
        for number, statement in statements(text.split("\n")):
            if bare(statement):
                found.append(f"{shown}:{number}: {statement}")
    return found


def default_files():
    files = glob.glob(
        os.path.join(ROOT, ".github", "denext-runtime", "**", "*.sh"), recursive=True
    )
    files += glob.glob(os.path.join(ROOT, ".github", "workflows", "denext_*.yml"))
    return sorted(files)


def main(argv):
    files = argv or default_files()
    found = [f for path in files for f in check_file(path)]
    for line in found:
        print(line)
    if found:
        print(
            f"::error::{len(found)} bare assertion(s): name what is checked "
            '(need "what" test ..., or ... || { echo "::error::..." >&2; exit 1; })',
            file=sys.stderr,
        )
        return 1
    print(f"assert_guard: {len(files)} files, no bare assertion")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
