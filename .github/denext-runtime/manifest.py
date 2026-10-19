#!/usr/bin/env python3
"""Write SHA256SUMS and manifest.json for a directory of runtime archives
(and the `deno-cli-*` CLI archives, listed under "cli", each with the
esbuild it ships: version and the binary's sha256, and the native TypeScript
compiler in its `typescript-check/`: version, the binary's sha256 and a sha256
over every file of the directory; and the `deno-denort-*`
archives `deno compile` builds onto, listed under "denort").

Usage: manifest.py <dist dir>
Env:   VERSION TAG REPOSITORY DENO_VERSION DENO_SHA LAUFEY_SHA CEF_VERSION RUN_URL
       (VERSION: the fork's own version, e.g. 3.0.0-rc.1, which names every
       archive; DENO_VERSION: the upstream Deno release it is built from)
       CLI_ONLY (1: a `cli_only` dry run, which has no runtime archives)
"""

import hashlib
import json
import os
import re
import sys
import zipfile

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

cli_pattern = re.compile(
    r"^deno-cli-"
    + re.escape(version)
    + r"-(?P<target>[a-z0-9_]+-[a-z0-9_]+-[a-z0-9_]+(?:-[a-z0-9_]+)?)\.(?P<ext>tar\.gz|zip)$"
)

denort_pattern = re.compile(
    r"^deno-denort-"
    + re.escape(version)
    + r"-(?P<target>[a-z0-9_]+-[a-z0-9_]+-[a-z0-9_]+(?:-[a-z0-9_]+)?)\.zip$"
)


def describe(name, ext):
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
    return {
        "file": name,
        "url": url,
        "sha256": digest,
        "size": os.path.getsize(path),
        "format": ext,
    }


def esbuild_of(name, target):
    """The esbuild inside a CLI zip (assemble_cli.sh): its version, from
    BUILD_INFO.json, and the sha256 of the binary itself."""
    binary = "esbuild.exe" if target.endswith("windows-msvc") else "esbuild"
    with zipfile.ZipFile(os.path.join(dist, name)) as z:
        names = set(z.namelist())
        for want in (binary, "esbuild.LICENSE", "BUILD_INFO.json"):
            if want not in names:
                sys.exit(f"{name} has no {want}")
        info = json.loads(z.read("BUILD_INFO.json")).get("esbuild") or {}
        h = hashlib.sha256()
        with z.open(binary) as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
    digest = h.hexdigest()
    if info.get("sha256") != digest:
        sys.exit(f"{name}: {binary} sha256 {digest} != BUILD_INFO.json {info.get('sha256')}")
    if not info.get("version"):
        sys.exit(f"{name}: BUILD_INFO.json names no esbuild version")
    return {"version": info["version"], "sha256": digest}


def typescript_check_of(name, target):
    """The native TypeScript compiler type checking runs, inside a CLI zip
    (assemble_cli.sh, `typescript-check/`): its version, from BUILD_INFO.json
    (fetch_typescript.py check), the sha256 of the binary and a tree sha256
    (`<name>\\0<sha256>\\n` per file but typescript.json, sorted), each
    recomputed from the zip: denext pins both."""
    binary = "tsc.exe" if target.endswith("windows-msvc") else "tsc"
    prefix = "typescript-check/"
    with zipfile.ZipFile(os.path.join(dist, name)) as z:
        names = set(z.namelist())
        for want in (prefix + binary, prefix + "LICENSE", prefix + "typescript.json", "BUILD_INFO.json"):
            if want not in names:
                sys.exit(f"{name} has no {want}")
        info = json.loads(z.read("BUILD_INFO.json")).get("typescript_check") or {}
        files = {}
        for n in sorted(names):
            if n.startswith(prefix) and not n.endswith("/") and n != prefix + "typescript.json":
                files[n[len(prefix):]] = hashlib.sha256(z.read(n)).hexdigest()
    tree = hashlib.sha256()
    for rel in sorted(files):
        tree.update(f"{rel}\0{files[rel]}\n".encode())
    if info.get("sha256") != files.get(binary):
        sys.exit(f"{name}: {prefix}{binary} sha256 {files.get(binary)} != BUILD_INFO.json {info.get('sha256')}")
    if not info.get("version"):
        sys.exit(f"{name}: BUILD_INFO.json names no typescript_check version")
    return {"version": info["version"], "sha256": files[binary], "tree_sha256": tree.hexdigest()}


targets = {}
cli_targets = {}
denort_targets = {}
sums = []
for name in sorted(os.listdir(dist)):
    m = pattern.match(name)
    if m:
        targets.setdefault(m["target"], {})[m["backend"]] = describe(name, m["ext"])
        continue
    m = denort_pattern.match(name)
    if m:
        binary = "denort.exe" if m["target"].endswith("windows-msvc") else "denort"
        with zipfile.ZipFile(os.path.join(dist, name)) as z:
            if z.namelist() != [binary]:
                sys.exit(f"{name} must hold just {binary}, not {z.namelist()}")
        denort_targets[m["target"]] = {"binary": binary, **describe(name, "zip")}
        continue
    m = cli_pattern.match(name)
    if m:
        cli_targets[m["target"]] = {
            "binary": "deno.exe" if m["target"].endswith("windows-msvc") else "deno",
            **describe(name, m["ext"]),
            "esbuild": esbuild_of(name, m["target"]),
            "typescript_check": typescript_check_of(name, m["target"]),
        }

if not targets and not (env.get("CLI_ONLY") == "1" and cli_targets):
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
    # This fork's own `deno` CLI, a zip per target with the binary at the
    # top: `deno --version` and Deno.version.deno are `version`.
    "cli": {
        "version": version,
        "targets": dict(sorted(cli_targets.items())),
    },
    # The standalone runtime the CLI's `deno compile` downloads from this
    # release (DENORT_BIN still overrides it): a zip per target holding just
    # the binary.
    "denort": {
        "version": version,
        "targets": dict(sorted(denort_targets.items())),
    },
}

with open(os.path.join(dist, "SHA256SUMS"), "w") as f:
    f.write("\n".join(sums) + "\n")
with open(os.path.join(dist, "manifest.json"), "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
print(json.dumps(manifest, indent=2))
