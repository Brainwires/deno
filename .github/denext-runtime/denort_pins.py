#!/usr/bin/env python3
"""The release's checks of the denort SHA-256s compiled into the CLI
(DENEXT_DENORT_SHA256S, DENEXT.md items 188 and 215).

check-list <list>
    A tag's `<target>=<sha256>,...` names exactly the release targets.
check-cli <version> <dist> [--release]
    Every `deno-cli-<version>-<target>.zip` in <dist> holds a `deno` binary
    carrying `<target>=<sha256>` for every `deno-denort-*` archive there,
    hashed from the archive's bytes. With --release both sets of archives are
    exactly the release targets.
check-manifest <manifest.json>
    A tag's manifest lists exactly the release targets: webview and cef
    runtime archives, a CLI archive and a denort archive for each.
"""
import hashlib
import json
import os
import re
import sys
import zipfile

# denext_runtime.yml's setup job builds these (denort_pins_test.py compares).
RELEASE_TARGETS = frozenset(
    {
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    }
)


class PinError(Exception):
    pass


def require_release_targets(what, targets):
    """`targets` (a list, so a duplicate shows) is exactly the release's."""
    seen = sorted(targets)
    if seen != sorted(RELEASE_TARGETS):
        missing = sorted(RELEASE_TARGETS - set(seen))
        extra = sorted(set(seen) - RELEASE_TARGETS)
        dupes = sorted({t for t in seen if seen.count(t) > 1})
        raise PinError(f"{what}: missing {missing}, unexpected {extra}, duplicated {dupes}")


def check_list(text):
    targets = []
    for entry in filter(None, (e.strip() for e in text.split(","))):
        target, _, digest = entry.partition("=")
        if not re.fullmatch(r"[0-9a-fA-F]{64}", digest):
            raise PinError(f"DENEXT_DENORT_SHA256S: malformed entry '{entry}'")
        targets.append(target)
    require_release_targets("DENEXT_DENORT_SHA256S", targets)


def archives(dist, prefix, version):
    """{target: file name} of `<prefix><version>-<target>.zip` in `dist`."""
    start, found = f"{prefix}{version}-", {}
    for name in os.listdir(dist):
        if name.startswith(start) and name.endswith(".zip"):
            found[name[len(start) : -len(".zip")]] = name
    return found


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def check_cli(version, dist, release=False):
    clis = archives(dist, "deno-cli-", version)
    denorts = archives(dist, "deno-denort-", version)
    if not clis:
        raise PinError(f"no deno-cli-{version}-*.zip in {dist}")
    if not denorts:
        raise PinError(f"no deno-denort-{version}-*.zip in {dist}")
    if release:
        require_release_targets("CLI archives", list(clis))
        require_release_targets("denort archives", list(denorts))
    entries = {
        target: f"{target}={sha256_file(os.path.join(dist, name))}".encode()
        for target, name in denorts.items()
    }
    for target, name in sorted(clis.items()):
        binary = "deno.exe" if target.endswith("windows-msvc") else "deno"
        with zipfile.ZipFile(os.path.join(dist, name)) as archive:
            data = archive.read(binary)
        # A CLI of a target the run built no denort for would have no pin.
        if target not in entries:
            raise PinError(f"{name}: no denort archive for {target}")
        absent = sorted(t for t, entry in entries.items() if entry not in data)
        if absent:
            raise PinError(f"{name}: {binary} does not carry the denort SHA-256 of {absent}")
        print(f"{name}: carries {len(entries)} denort SHA-256s")


def check_manifest(manifest):
    runtimes = manifest["targets"]
    require_release_targets("desktop runtime archives", list(runtimes))
    for target, entry in runtimes.items():
        backends = sorted(k for k in entry if k != "runtimeLib")
        if backends != ["cef", "webview"]:
            raise PinError(f"{target}: runtime backends {backends}, want cef + webview")
    require_release_targets("CLI archives", list(manifest["cli"]["targets"]))
    require_release_targets("denort archives", list(manifest["denort"]["targets"]))


def main(argv):
    command, args = (argv[0], argv[1:]) if argv else (None, [])
    if command == "check-list" and len(args) == 1:
        check_list(args[0])
    elif command == "check-cli" and len(args) in (2, 3) and args[2:] in ([], ["--release"]):
        check_cli(args[0], args[1], release=len(args) == 3)
    elif command == "check-manifest" and len(args) == 1:
        with open(args[0]) as f:
            check_manifest(json.load(f))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except PinError as err:
        print(f"::error::{err}", file=sys.stderr)
        raise SystemExit(1)
