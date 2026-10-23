#!/usr/bin/env python3
"""Fetch the official tsgolint build for a target from the npm registry, as
pinned in cli/tools/lint/tsgolint_pin.json (the binary `deno lint
--type-aware` runs; see cli/tools/lint/tsgolint.rs).

Used by the `deno-cli-*` archive (assemble_cli.sh ships it next to `deno` as
`tsgolint/`, which `deno` prefers over a download) and by the test harness
(tests/util/lib/lib.rs, `tsgolint_bin`).

Every tarball is pinned twice: by its npm `dist.integrity` (sha512), which the
downloaded bytes must hash to, and by the sha256 of the binary inside it.
Unless --no-registry-check, the registry's current metadata must still name
the same tarball and integrity, or this fails.

Usage: fetch_tsgolint.py [--no-registry-check] <target> <out dir>
         <target>: a Rust target triple (x86_64-unknown-linux-gnu, ...) or an
                   npm platform suffix (linux-x64, ...).
       Writes, flattened into <out dir>: tsgolint[.exe], LICENSE and
       tsgolint.json (version, binary sha256, sources). The directory is
       replaced atomically.
       fetch_tsgolint.py --pin <version>
       Rewrites the pin file for <version>: every platform's tarball URL and
       integrity from the registry, and the sha256 of each binary (all six
       tarballs are downloaded and verified).
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
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
PIN = os.path.join(HERE, "..", "..", "cli", "tools", "lint", "tsgolint_pin.json")
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


def package_of(platform):
    return f"@oxlint-tsgolint/{platform}"


def exe_of(platform):
    return "tsgolint.exe" if platform.startswith("win32") else "tsgolint"


def registry_dist(platform, version):
    package = package_of(platform)
    meta = json.loads(get(f"{REGISTRY}/{urllib.parse.quote(package, safe='@')}/{version}"))
    if meta.get("version") != version:
        raise SystemExit(f"{package}: registry version {meta.get('version')}, want {version}")
    return meta.get("dist") or {}


def verified_tarball(platform, tarball, integrity):
    data = get(tarball)
    algo, _, want = integrity.partition("-")
    if algo != "sha512":
        raise SystemExit(f"{package_of(platform)}: unsupported integrity {integrity}")
    got = base64.b64encode(hashlib.sha512(data).digest()).decode()
    if got != want:
        raise SystemExit(f"{package_of(platform)}: tarball sha512-{got} does not match {integrity}")
    return data


def unpack(platform, data):
    """{relative name: bytes} for the binary and LICENSE (package/<name>)."""
    exe = exe_of(platform)
    files = {}
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar.getmembers():
            if member.isfile() and member.name in (f"package/{exe}", "package/LICENSE"):
                files[member.name[len("package/"):]] = tar.extractfile(member).read()
    if exe not in files:
        raise SystemExit(f"{package_of(platform)}: no package/{exe} in the tarball")
    if "LICENSE" not in files:
        raise SystemExit(f"{package_of(platform)}: no package/LICENSE in the tarball")
    return files


def write_pin(version):
    platforms = {}
    for platform in sorted(TARGETS.values()):
        dist = registry_dist(platform, version)
        tarball, integrity = dist.get("tarball"), dist.get("integrity")
        if not tarball or not integrity:
            raise SystemExit(f"{package_of(platform)}@{version}: the registry names no tarball/integrity")
        files = unpack(platform, verified_tarball(platform, tarball, integrity))
        platforms[platform] = {
            "tarball": tarball,
            "integrity": integrity,
            "sha256": hashlib.sha256(files[exe_of(platform)]).hexdigest(),
        }
        print(f"verified {tarball}", file=sys.stderr)
    with open(PIN, encoding="utf-8") as f:
        pin = json.load(f)
    pin["version"] = version
    pin["platforms"] = platforms
    with open(PIN, "w", encoding="utf-8") as f:
        json.dump(pin, f, indent=2)
        f.write("\n")
    print(f"pinned tsgolint {version} in {os.path.relpath(PIN)}")


def fetch(target, out_dir, registry_check):
    platform = TARGETS.get(target, target)
    if platform not in TARGETS.values():
        raise SystemExit(f"no tsgolint build for target {target}")
    with open(PIN, encoding="utf-8") as f:
        pin = json.load(f)
    version = pin["version"]
    entry = pin["platforms"][platform]
    tarball, integrity, sha256 = entry["tarball"], entry["integrity"], entry["sha256"]
    package = package_of(platform)

    if registry_check:
        dist = registry_dist(platform, version)
        if dist.get("tarball") != tarball:
            raise SystemExit(f"{package}: registry tarball {dist.get('tarball')}, pinned {tarball}")
        if dist.get("integrity") != integrity:
            raise SystemExit(f"{package}: registry integrity {dist.get('integrity')}, pinned {integrity}")

    files = unpack(platform, verified_tarball(platform, tarball, integrity))
    exe = exe_of(platform)
    got = hashlib.sha256(files[exe]).hexdigest()
    if got != sha256:
        raise SystemExit(f"{package}: {exe} sha256 {got} does not match the pinned {sha256}")
    print(f"verified {tarball} ({integrity}, {exe} sha256 {sha256})")

    out_dir = os.path.abspath(out_dir)
    os.makedirs(os.path.dirname(out_dir), exist_ok=True)
    tmp = f"{out_dir}.{os.getpid()}.tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp)
    for rel, data in files.items():
        dest = os.path.join(tmp, rel)
        with open(dest, "wb") as f:
            f.write(data)
        if rel == exe:
            os.chmod(dest, os.stat(dest).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    info = {
        "version": version,
        "binary": exe,
        "sha256": sha256,
        "package": f"{package}@{version}",
        "tarball": tarball,
        "integrity": integrity,
        "license": {"file": "LICENSE", "spdx": "MIT"},
    }
    with open(os.path.join(tmp, "tsgolint.json"), "w", encoding="utf-8") as f:
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


def main():
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == "--pin":
        write_pin(args[1])
        return
    registry_check = True
    if args and args[0] == "--no-registry-check":
        registry_check = False
        args = args[1:]
    if len(args) != 2:
        raise SystemExit(__doc__)
    fetch(args[0], args[1], registry_check)


main()
