#!/usr/bin/env python3
"""Fetch the official esbuild binary for a target from the npm registry, for
the `deno-cli-*` archive (assemble_cli.sh): `deno bundle` uses an esbuild
next to the `deno` executable instead of downloading one (DENEXT.md).

Every tarball is pinned below by its npm `dist.integrity` (sha512): the
registry's current packument must still name the same tarball and integrity,
and the downloaded bytes must hash to it, or this fails.

Usage: fetch_esbuild.py <target> <out dir>
Writes <out dir>/esbuild[.exe] (the binary), <out dir>/esbuild.LICENSE
(esbuild's MIT license, from the `esbuild` package of the same version) and
<out dir>/esbuild.json (version, sources, binary sha256).
Env: ESBUILD_VERSION (must equal the version pinned here)
"""

import base64
import hashlib
import io
import json
import os
import stat
import sys
import tarfile
import time
import urllib.request

VERSION = "0.25.5"
REGISTRY = "https://registry.npmjs.org"

# npm package -> (tarball URL, dist.integrity), esbuild 0.25.5.
PACKAGES = {
    "@esbuild/darwin-arm64": (
        "https://registry.npmjs.org/@esbuild/darwin-arm64/-/darwin-arm64-0.25.5.tgz",
        "sha512-GtaBgammVvdF7aPIgH2jxMDdivezgFu6iKpmT+48+F8Hhg5J/sfnDieg0aeG/jfSvkYQU2/pceFPDKlqZzwnfQ==",
    ),
    "@esbuild/darwin-x64": (
        "https://registry.npmjs.org/@esbuild/darwin-x64/-/darwin-x64-0.25.5.tgz",
        "sha512-1iT4FVL0dJ76/q1wd7XDsXrSW+oLoquptvh4CLR4kITDtqi2e/xwXwdCVH8hVHU43wgJdsq7Gxuzcs6Iq/7bxQ==",
    ),
    "@esbuild/linux-x64": (
        "https://registry.npmjs.org/@esbuild/linux-x64/-/linux-x64-0.25.5.tgz",
        "sha512-uhj8N2obKTE6pSZ+aMUbqq+1nXxNjZIIjCjGLfsWvVpy7gKCOL6rsY1MhRh9zLtUtAI7vpgLMK6DxjO8Qm9lJw==",
    ),
    "@esbuild/linux-arm64": (
        "https://registry.npmjs.org/@esbuild/linux-arm64/-/linux-arm64-0.25.5.tgz",
        "sha512-Z9kfb1v6ZlGbWj8EJk9T6czVEjjq2ntSYLY2cw6pAZl4oKtfgQuS4HOq41M/BcoLPzrUbNd+R4BXFyH//nHxVg==",
    ),
    "@esbuild/win32-x64": (
        "https://registry.npmjs.org/@esbuild/win32-x64/-/win32-x64-0.25.5.tgz",
        "sha512-TXv6YnJ8ZMVdX+SXWVBo/0p8LTcrUYngpWjvm91TMjjBQii7Oz11Lw5lbDV5Y0TzuhSJHwiH4hEtC1I42mMS0g==",
    ),
    # The main package, for its LICENSE.md (the platform packages carry none).
    "esbuild": (
        "https://registry.npmjs.org/esbuild/-/esbuild-0.25.5.tgz",
        "sha512-P8OtKZRv/5J5hhz0cUAdu/cLuPIKXpQl1R9pZtvmHWQvrAUVd0UNIPT4IB4W3rNOqVO0rlqHmCIbSwxh/c9yUQ==",
    ),
}

# Rust target triple -> (npm package, binary path inside the tarball).
TARGETS = {
    "aarch64-apple-darwin": ("@esbuild/darwin-arm64", "package/bin/esbuild"),
    "x86_64-apple-darwin": ("@esbuild/darwin-x64", "package/bin/esbuild"),
    "x86_64-unknown-linux-gnu": ("@esbuild/linux-x64", "package/bin/esbuild"),
    "aarch64-unknown-linux-gnu": ("@esbuild/linux-arm64", "package/bin/esbuild"),
    "x86_64-pc-windows-msvc": ("@esbuild/win32-x64", "package/esbuild.exe"),
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


def fetch_verified(package):
    tarball, integrity = PACKAGES[package]
    # The registry must still describe this exact version the same way.
    meta = json.loads(get(f"{REGISTRY}/{package}/{VERSION}"))
    dist = meta.get("dist") or {}
    if meta.get("version") != VERSION:
        raise SystemExit(f"{package}: registry version {meta.get('version')}, want {VERSION}")
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
    return tarball, integrity, data


def member(data, name):
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        f = tar.extractfile(name)
        if f is None:
            raise SystemExit(f"{name} is not a file in the tarball")
        return f.read()


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    target, out_dir = sys.argv[1], sys.argv[2]
    env_version = os.environ.get("ESBUILD_VERSION", VERSION)
    if env_version != VERSION:
        raise SystemExit(f"ESBUILD_VERSION={env_version} but fetch_esbuild.py pins {VERSION}")
    if target not in TARGETS:
        raise SystemExit(f"no esbuild package for target {target}")
    package, path = TARGETS[target]
    os.makedirs(out_dir, exist_ok=True)

    tarball, integrity, data = fetch_verified(package)
    binary = member(data, path)
    bin_name = "esbuild.exe" if path.endswith(".exe") else "esbuild"
    bin_path = os.path.join(out_dir, bin_name)
    with open(bin_path, "wb") as f:
        f.write(binary)
    os.chmod(bin_path, os.stat(bin_path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    lic_tarball, lic_integrity, lic_data = fetch_verified("esbuild")
    with open(os.path.join(out_dir, "esbuild.LICENSE"), "wb") as f:
        f.write(member(lic_data, "package/LICENSE.md"))

    info = {
        "version": VERSION,
        "binary": bin_name,
        "sha256": hashlib.sha256(binary).hexdigest(),
        "package": f"{package}@{VERSION}",
        "tarball": tarball,
        "integrity": integrity,
        "license": {"file": "esbuild.LICENSE", "tarball": lic_tarball, "integrity": lic_integrity},
    }
    with open(os.path.join(out_dir, "esbuild.json"), "w") as f:
        json.dump(info, f, indent=2)
        f.write("\n")
    print(json.dumps(info, indent=2))


main()
