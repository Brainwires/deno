#!/usr/bin/env python3
"""Fetch the official native TypeScript (tsgo) build for a target from the npm
registry, as pinned in cli/tsc/typescript_pins.json (the one pin mechanism for
every native TypeScript this fork runs; see cli/tsc/native_pin.rs).

Used by the `deno-cli-*` archive (assemble_cli.sh ships it next to `deno` as
`typescript-<role>/`, which `deno` prefers over a download) and by the test
harness (tests/util/lib/lsp.rs) for the tsgo LSP tests.

Every tarball is pinned by its npm `dist.integrity` (sha512): the downloaded
bytes must hash to it, and (unless --no-registry-check) the registry's current
metadata must still name the same tarball and integrity, or this fails.

Usage: fetch_typescript.py [--no-registry-check] <role> <target> <out dir>
  <target>: a Rust target triple (x86_64-unknown-linux-gnu, ...) or an npm
            platform suffix (linux-x64, ...).
Writes, flattened into <out dir>: tsc[.exe], its lib.*.d.ts, LICENSE,
NOTICE.txt and typescript.json (role, version, binary sha256, sources).
The directory is replaced atomically.
"""

import base64
import hashlib
import io
import json
import os
import shutil
import stat
import sys
import tarfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
PINS = os.path.join(HERE, "..", "..", "cli", "tsc", "typescript_pins.json")
REGISTRY = "https://registry.npmjs.org"

# Rust target triple -> npm platform suffix.
TARGETS = {
    "aarch64-apple-darwin": "darwin-arm64",
    "x86_64-apple-darwin": "darwin-x64",
    "x86_64-unknown-linux-gnu": "linux-x64",
    "aarch64-unknown-linux-gnu": "linux-arm64",
    "x86_64-pc-windows-msvc": "win32-x64",
    "aarch64-pc-windows-msvc": "win32-arm64",
}


def get(url):
    last = None
    for attempt in range(1, 4):
        try:
            with urllib.request.urlopen(url, timeout=120) as r:
                return r.read()
        except Exception as e:  # noqa: BLE001 - retried, then raised
            last = e
            print(f"GET {url} failed (attempt {attempt}/3): {e}", file=sys.stderr)
            time.sleep(3 * attempt)
    raise SystemExit(f"could not download {url}: {last}")


def main():
    args = sys.argv[1:]
    registry_check = True
    if args and args[0] == "--no-registry-check":
        registry_check = False
        args = args[1:]
    if len(args) != 3:
        raise SystemExit(__doc__)
    role, target, out_dir = args
    platform = TARGETS.get(target, target)
    if platform not in TARGETS.values():
        raise SystemExit(f"no native TypeScript build for target {target}")
    with open(PINS, encoding="utf-8") as f:
        pins = json.load(f)
    if role not in pins or role.startswith("$"):
        raise SystemExit(f"typescript_pins.json has no role {role!r}")
    pin = pins[role]
    version = pin["version"]
    entry = pin["platforms"][platform]
    tarball, integrity = entry["tarball"], entry["integrity"]
    package = f"@typescript/typescript-{platform}"

    if registry_check:
        meta = json.loads(get(f"{REGISTRY}/{package}/{version}"))
        dist = meta.get("dist") or {}
        if meta.get("version") != version:
            raise SystemExit(f"{package}: registry version {meta.get('version')}, want {version}")
        if dist.get("tarball") != tarball:
            raise SystemExit(f"{package}: registry tarball {dist.get('tarball')}, pinned {tarball}")
        if dist.get("integrity") != integrity:
            raise SystemExit(f"{package}: registry integrity {dist.get('integrity')}, pinned {integrity}")

    data = get(tarball)
    algo, _, want = integrity.partition("-")
    assert algo == "sha512", integrity
    got = base64.b64encode(hashlib.sha512(data).digest()).decode()
    if got != want:
        raise SystemExit(f"{package}: tarball sha512-{got} does not match {integrity}")
    print(f"verified {tarball} ({integrity})")

    exe = "tsc.exe" if platform.startswith("win32") else "tsc"
    out_dir = os.path.abspath(out_dir)
    parent = os.path.dirname(out_dir)
    os.makedirs(parent, exist_ok=True)
    tmp = f"{out_dir}.{os.getpid()}.tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp)
    found = False
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar.getmembers():
            if not member.isfile():
                continue
            name = member.name
            if name.startswith("package/lib/"):
                rel = name[len("package/lib/"):]
            elif name in ("package/LICENSE", "package/NOTICE.txt"):
                rel = name[len("package/"):]
            else:
                continue
            parts = rel.split("/")
            if any(p in ("", ".", "..") for p in parts):
                raise SystemExit(f"unexpected path in tarball: {name}")
            dest = os.path.join(tmp, *parts)
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(dest, "wb") as f:
                f.write(tar.extractfile(member).read())
            if rel == exe:
                found = True
                os.chmod(dest, os.stat(dest).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    if not found:
        shutil.rmtree(tmp, ignore_errors=True)
        raise SystemExit(f"{package}: no package/lib/{exe} in the tarball")

    with open(os.path.join(tmp, exe), "rb") as f:
        exe_sha = hashlib.sha256(f.read()).hexdigest()
    info = {
        "role": role,
        "version": version,
        "binary": exe,
        "sha256": exe_sha,
        "package": f"{package}@{version}",
        "tarball": tarball,
        "integrity": integrity,
        "license": {"file": "LICENSE", "notice": "NOTICE.txt", "spdx": "Apache-2.0"},
    }
    with open(os.path.join(tmp, "typescript.json"), "w", encoding="utf-8") as f:
        json.dump(info, f, indent=2)
        f.write("\n")

    # Replace atomically: a concurrent reader sees the old tree or the new one.
    old = f"{out_dir}.{os.getpid()}.old"
    if os.path.exists(out_dir):
        os.rename(out_dir, old)
    try:
        os.rename(tmp, out_dir)
    except OSError:
        if os.path.isfile(os.path.join(out_dir, exe)):
            shutil.rmtree(tmp, ignore_errors=True)  # another process won
        else:
            raise
    shutil.rmtree(old, ignore_errors=True)
    print(json.dumps(info, indent=2))


main()
