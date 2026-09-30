#!/usr/bin/env python3
"""Write SHA256SUMS and manifest.json for a directory of runtime archives.

Usage: manifest.py <dist dir>
Env:   VERSION TAG REPOSITORY DENO_VERSION DENO_SHA LAUFEY_SHA CEF_VERSION RUN_URL
"""

import hashlib
import json
import os
import re
import sys

dist = sys.argv[1]
env = os.environ
version = env["VERSION"]
tag = env.get("TAG", "")
repo = env["REPOSITORY"]

pattern = re.compile(
    r"^deno-desktop-runtime-"
    + re.escape(version)
    + r"-(?P<target>[a-z0-9_]+-[a-z0-9_]+-[a-z0-9_]+(?:-[a-z0-9_]+)?)-(?P<backend>webview|cef)\.(?P<ext>tar\.gz|zip)$"
)

targets = {}
sums = []
for name in sorted(os.listdir(dist)):
    m = pattern.match(name)
    if not m:
        continue
    path = os.path.join(dist, name)
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    digest = h.hexdigest()
    sums.append(f"{digest}  {name}")
    url = (
        f"https://github.com/{repo}/releases/download/{tag}/{name}"
        if tag
        else None
    )
    targets.setdefault(m["target"], {})[m["backend"]] = {
        "file": name,
        "url": url,
        "sha256": digest,
        "size": os.path.getsize(path),
        "format": m["ext"],
    }

if not targets:
    sys.exit(f"no runtime archives for version {version} in {dist}")

lib_names = {
    "apple-darwin": "libdenort.dylib",
    "linux-gnu": "libdenort.so",
    "windows-msvc": "denort.dll",
}


def lib_for(target):
    for suffix, lib in lib_names.items():
        if target.endswith(suffix):
            return lib
    raise SystemExit(f"unknown target {target}")


manifest = {
    "schema": 1,
    "name": "deno-desktop-runtime",
    "version": version,
    "tag": tag or None,
    "repository": f"https://github.com/{repo}",
    "deno": {
        "version": env["DENO_VERSION"],
        "sha": env["DENO_SHA"],
        "cli": f"stock deno {env['DENO_VERSION']}",
    },
    "laufey": {
        "repository": "https://github.com/Brainwires/laufey",
        "sha": env["LAUFEY_SHA"],
        "apiVersion": int(env["LAUFEY_API_VERSION"]) if env.get("LAUFEY_API_VERSION") else None,
    },
    "cef": {"version": env["CEF_VERSION"]},
    "layout": {
        "DENORT_DESKTOP_BIN": "<unpacked>/<runtimeLib>",
        "LAUFEY_DEV_DIR": "<unpacked>/laufey",
    },
    "build": {"run": env.get("RUN_URL")},
    "targets": {
        t: {"runtimeLib": lib_for(t), **backends}
        for t, backends in sorted(targets.items())
    },
}

with open(os.path.join(dist, "SHA256SUMS"), "w") as f:
    f.write("\n".join(sums) + "\n")
with open(os.path.join(dist, "manifest.json"), "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
print(json.dumps(manifest, indent=2))
