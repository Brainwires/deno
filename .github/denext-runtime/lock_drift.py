# Copyright 2018-2026 the Deno authors. MIT license.
"""Fail when a Cargo.lock changed beyond the `laufey` package.

The runtime build patches the `laufey` crate to a laufey checkout, which
rewrites laufey's own entry (source, version, checksum) and the references to
it. Every other package must stay exactly as locked: name, version, source,
checksum and dependency list. (The check this replaces only compared the
`name = ` lines, so a version or source change of any other crate passed.)

Usage: lock_drift.py <Cargo.lock before> <Cargo.lock after>
"""

import re
import sys


def packages(text):
  """{(name, version, source): normalized block} of every non-laufey package,
  with references to laufey reduced to its name."""
  out = {}
  for block in text.split("[[package]]")[1:]:
    fields = dict(re.findall(r'^(name|version|source) = "([^"]*)"$', block,
                             re.M))
    if fields.get("name") == "laufey":
      continue
    normalized = re.sub(r'"laufey(?: [^"]*)?"', '"laufey"', block.strip())
    key = (fields.get("name"), fields.get("version"), fields.get("source"))
    out[key] = normalized
  return out


def main(before_path, after_path):
  with open(before_path, encoding="utf-8") as f:
    before = packages(f.read())
  with open(after_path, encoding="utf-8") as f:
    after = packages(f.read())
  bad = []
  for key in sorted(set(before) | set(after), key=str):
    if key not in after:
      bad.append(f"removed: {key}")
    elif key not in before:
      bad.append(f"added: {key}")
    elif before[key] != after[key]:
      bad.append(f"changed: {key}")
  for line in bad:
    print(f"::error::Cargo.lock changed beyond laufey: {line}")
  return 1 if bad else 0


if __name__ == "__main__":
  sys.exit(main(sys.argv[1], sys.argv[2]))
