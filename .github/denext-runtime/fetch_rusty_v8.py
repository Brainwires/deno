#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Fetch and verify the prebuilt V8 the `v8` crate links (DENEXT.md item 162).

The `v8` crate comes from Brainwires/rusty_v8 (Cargo.toml's
[patch.crates-io]), and its build script would otherwise download the static
library and the bindgen output from denoland's releases, under a version that
does not exist there. This script takes both files for one target from the
Brainwires release named in rusty_v8.sha256 (next to this file), checks each
against the SHA-256 recorded there, and hands them to the build script:

  RUSTY_V8_ARCHIVE           the .a.gz / .lib.gz
  RUSTY_V8_ARCHIVE_SHA256    its pinned hash (build.rs checks it again)
  RUSTY_V8_SRC_BINDING_PATH  src_binding_release_<target>.rs

It fails closed: a target the manifest does not list, a download that fails, a
hash that differs, a manifest tag that does not match Cargo.lock's `v8`
version, or a RUSTY_V8_MIRROR in the environment all exit 1, and nothing is
written. The manifest is reviewed in the change that bumps V8.

Usage:
  fetch_rusty_v8.py --target <triple> --dest <dir> [--from <dir>]
                    [--github-env | --print-env]

  --from <dir>    take the files from a local directory (a build's output, an
                  offline mirror) instead of the release; they are verified
                  the same way.
  --github-env    append the three variables to $GITHUB_ENV.
  --print-env     print `export` lines for a POSIX shell.
Exits 0 on success, 1 on any failed check, 2 on a usage error.
"""

import argparse
import hashlib
import os
import re
import shutil
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
MANIFEST = os.path.join(HERE, "rusty_v8.sha256")
LOCKFILE = os.path.join(ROOT, "Cargo.lock")

_SHA = re.compile(r"^([0-9a-f]{64})\s+\*?(\S+)$")
_HEADER = re.compile(r"^#\s*(repo|tag):\s*(\S+)\s*$")


class Failure(Exception):
  pass


def parse_manifest(text):
  """{"repo", "tag", "files": {name: sha256}} from rusty_v8.sha256."""
  out = {"repo": None, "tag": None, "files": {}}
  for number, raw in enumerate(text.splitlines(), 1):
    line = raw.strip()
    if not line:
      continue
    header = _HEADER.match(line)
    if header:
      out[header.group(1)] = header.group(2)
      continue
    if line.startswith("#"):
      continue
    entry = _SHA.match(line)
    if not entry:
      raise Failure(f"rusty_v8.sha256:{number}: not `<sha256>  <file>`: {raw!r}")
    name = entry.group(2)
    if name in out["files"]:
      raise Failure(f"rusty_v8.sha256:{number}: {name} is listed twice")
    out["files"][name] = entry.group(1)
  if not out["repo"] or not out["tag"]:
    raise Failure("rusty_v8.sha256: needs `# repo:` and `# tag:` lines")
  return out


def artifact_names(target):
  """(static library archive, src binding) release file names for `target`."""
  if "windows" in target:
    lib = f"rusty_v8_release_{target}.lib.gz"
  else:
    lib = f"librusty_v8_release_{target}.a.gz"
  return lib, f"src_binding_release_{target}.rs"


def lock_v8_version(lock_text):
  """The `version` of the `v8` package in Cargo.lock."""
  match = re.search(
    r'^\[\[package\]\]\nname = "v8"\nversion = "([^"]+)"$', lock_text, re.M
  )
  if not match:
    raise Failure("Cargo.lock has no `v8` package")
  return match.group(1)


def check_tag(tag, version):
  """The release tag must be v<version without metadata>-<metadata>:
  152.2.1+denext.1 <-> v152.2.1-denext.1."""
  base, _, meta = version.partition("+")
  want = f"v{base}-{meta}" if meta else None
  if tag != want:
    raise Failure(
      f"rusty_v8.sha256 names release {tag}, but Cargo.lock's v8 is "
      f"{version} (expected tag {want or 'v<version>-denext.<n>'})"
    )


def sha256_of(path):
  digest = hashlib.sha256()
  with open(path, "rb") as f:
    for block in iter(lambda: f.read(1 << 20), b""):
      digest.update(block)
  return digest.hexdigest()


def download(url, dest, attempts=4):
  last = None
  for attempt in range(attempts):
    try:
      with urllib.request.urlopen(url, timeout=120) as response:
        with open(dest, "wb") as out:
          shutil.copyfileobj(response, out)
      return
    except OSError as error:
      last = error
      time.sleep(2**attempt)
  raise Failure(f"download failed: {url}: {last}")


def fetch(target, dest, source_dir=None, manifest_text=None, lock_text=None,
          fetcher=download):
  """Place and verify both files for `target` in `dest`; returns the
  environment for the build script."""
  if os.environ.get("RUSTY_V8_MIRROR"):
    raise Failure("RUSTY_V8_MIRROR is set; unset it so nothing else is fetched")
  if manifest_text is None:
    with open(MANIFEST, encoding="utf-8") as f:
      manifest_text = f.read()
  if lock_text is None:
    with open(LOCKFILE, encoding="utf-8") as f:
      lock_text = f.read()
  manifest = parse_manifest(manifest_text)
  check_tag(manifest["tag"], lock_v8_version(lock_text))
  os.makedirs(dest, exist_ok=True)
  paths = []
  for name in artifact_names(target):
    want = manifest["files"].get(name)
    if not want:
      raise Failure(f"rusty_v8.sha256 lists no {name} (target {target})")
    path = os.path.join(dest, name)
    if source_dir:
      shutil.copyfile(os.path.join(source_dir, name), path)
    else:
      url = (
        f"https://github.com/{manifest['repo']}/releases/download/"
        f"{manifest['tag']}/{name}"
      )
      fetcher(url, path)
    got = sha256_of(path)
    if got != want:
      os.remove(path)
      raise Failure(f"{name}: SHA-256 {got}, rusty_v8.sha256 pins {want}")
    print(f"{name}: SHA-256 OK ({got})", file=sys.stderr)
    paths.append((os.path.abspath(path), got))
  (lib, lib_sha), (binding, _) = paths
  return {
    "RUSTY_V8_ARCHIVE": lib,
    "RUSTY_V8_ARCHIVE_SHA256": lib_sha,
    "RUSTY_V8_SRC_BINDING_PATH": binding,
  }


def main(argv):
  parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
  parser.add_argument("--target", required=True)
  parser.add_argument("--dest", required=True)
  parser.add_argument("--from", dest="source_dir")
  mode = parser.add_mutually_exclusive_group()
  mode.add_argument("--github-env", action="store_true")
  mode.add_argument("--print-env", action="store_true")
  args = parser.parse_args(argv)
  try:
    env = fetch(args.target, args.dest, args.source_dir)
  except (Failure, OSError) as error:
    print(f"::error::fetch_rusty_v8: {error}", file=sys.stderr)
    return 1
  if args.github_env:
    with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as f:
      for key, value in env.items():
        f.write(f"{key}={value}\n")
  for key, value in env.items():
    if args.print_env:
      print(f"export {key}='{value}'")
    else:
      print(f"{key}={value}", file=sys.stderr)
  return 0


if __name__ == "__main__":
  sys.exit(main(sys.argv[1:]))
