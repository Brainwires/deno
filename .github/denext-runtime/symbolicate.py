#!/usr/bin/env python3
"""Symbolicate a stack trace from a denext runtime release (DENEXT.md item
181), and check that a release carries the symbols it takes.

A release build's panic banner (`Stack trace: <trace> ...`) and its crash
reports (`"trace"`, DENEXT.md item 180) carry `deno_panic`'s trace: base64url
VLQ numbers, one per frame, each an address relative to the binary's image
(Linux: the load bias; macOS: `__TEXT` vmaddr + slide; Windows: the module
base). This tool decodes it, downloads the release's binary and symbol
archives (`deno-symbols-*`, `denort-symbols-*` / `deno-desktop-runtime-symbols-*`), checks them
against the release's SHA256SUMS / SYMBOLS.SHA256SUMS, and runs
`llvm-symbolizer` on the addresses:

  Linux    --obj=<binary> --dwp=<binary>.dwp <address>
  macOS    --obj=<binary>.dSYM/Contents/Resources/DWARF/<binary>
           <0x100000000 + address> (the trace leaves `__TEXT`'s vmaddr out)
  Windows  --obj=<binary> --relative-address <address>, the .pdb beside it

A maintainer tool: run it on a Linux machine with LLVM (biscuits), not on a
release runner. No network is needed with `--from <dir>` (the archives
already downloaded).

Usage:
  symbolicate.py <version> <target> <trace> [--binary deno|denort|libdenort]
                 [--from <dir>] [--llvm-symbolizer <path>] [--addresses]
  symbolicate.py check-release <version> [--assets <file.json> | --dir <dir>]

`--addresses` prints the decoded addresses only. `check-release` lists what a
release lacks (a symbol archive per target and binary kind, the checksum file)
or carries empty (a macOS archive of a dangling `.dSYM` symlink); `--assets`
reads `gh release view --json assets` output instead of asking GitHub, and
`--dir` a directory of the files (denext_runtime.yml's publish job, before it
publishes). Exits 1 when it finds a problem.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile

REPO = "Brainwires/deno"
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-pc-windows-msvc",
    "x86_64-pc-windows-msvc",
)
# A symbol archive smaller than this holds no debug info (an empty
# directory, a symlink): the smallest real one, a libdenort .dSYM, is MBs.
MIN_SYMBOLS_BYTES = 64 * 1024
# The image base a macOS executable's `__TEXT` segment is linked at.
MACOS_TEXT_VMADDR = 0x100000000
BASE64URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"


def vlq_decode(trace):
    """The numbers in a `deno_panic` trace (deno_panic::vlq): 5 bits per
    base64url digit, low groups first, 0x20 marks a continuation, and the
    lowest bit of the result is the sign."""
    values = []
    result = shift = 0
    pending = False
    for ch in trace.strip():
        digit = BASE64URL.find(ch)
        if digit < 0:
            raise ValueError(f"not a deno_panic trace character: {ch!r}")
        result |= (digit & 0x1F) << shift
        shift += 5
        pending = True
        if digit & 0x20 == 0:
            values.append(-(result >> 1) if result & 1 else result >> 1)
            result = shift = 0
            pending = False
    if pending:
        raise ValueError("the trace ends inside a number")
    return values


def vlq_encode(value):
    """`deno_panic`'s encoding of one number (for tests)."""
    vlq = (value << 1) if value >= 0 else ((-value << 1) | 1)
    out = []
    while True:
        digit = vlq & 0x1F
        vlq >>= 5
        if vlq:
            digit |= 0x20
        out.append(BASE64URL[digit])
        if not vlq:
            return "".join(out)


def tag(version):
    return f"denext-runtime-v{version}"


# The first release whose `denort` is built in its own job (DENEXT.md item
# 188: the CLI is compiled with the denort archives' SHA-256s), so its debug
# info is in `denort-symbols-*`; earlier releases put it in `deno-symbols-*`
# beside the CLI's.
DENORT_SYMBOLS_SINCE = (3, 0, 0, 8)


def separate_denort_symbols(version):
    """Whether a release of `version` keeps denort's debug info in its own
    `denort-symbols-*` archive. `3.0.0-denext.N` from N = 8, and any later
    `x.y.z`; anything older or unparsable (`2.9.7-denext.22`, `3.0.0-rc.7`)
    does not."""
    m = re.match(r"^(\d+)\.(\d+)\.(\d+)(?:-denext\.(\d+))?", version)
    if not m:
        return False
    core = tuple(int(g) for g in m.groups()[:3])
    since = DENORT_SYMBOLS_SINCE
    if core != since[:3]:
        return core > since[:3]
    return m[4] is not None and int(m[4]) >= since[3]


def symbol_assets(version):
    """The symbol archives a release of `version` should carry."""
    names = []
    for target in TARGETS:
        names.append(f"deno-symbols-{version}-{target}.tar.gz")
        if separate_denort_symbols(version):
            names.append(f"denort-symbols-{version}-{target}.tar.gz")
        names.append(f"deno-desktop-runtime-symbols-{version}-{target}.tar.gz")
    return names


def check_release(version, assets):
    """The problems of a release whose assets are {name: size}: a missing or
    empty symbol archive, or no SYMBOLS.SHA256SUMS."""
    problems = []
    for name in symbol_assets(version):
        if name not in assets:
            problems.append(f"missing: {name}")
        elif assets[name] < MIN_SYMBOLS_BYTES:
            problems.append(f"empty: {name} ({assets[name]} bytes, no debug info)")
    if "SYMBOLS.SHA256SUMS" not in assets:
        problems.append("missing: SYMBOLS.SHA256SUMS")
    return problems


def is_windows(target):
    return target.endswith("-windows-msvc")


def is_macos(target):
    return target.endswith("-apple-darwin")


def binary_file(binary, target):
    """The file name of `binary` (deno, denort, libdenort) on `target`."""
    if binary == "libdenort":
        if is_windows(target):
            return "denort.dll"
        return "libdenort.dylib" if is_macos(target) else "libdenort.so"
    return f"{binary}.exe" if is_windows(target) else binary


def debug_file(binary, target):
    """The debug file of `binary` inside its symbol archive."""
    name = binary_file(binary, target)
    if is_macos(target):
        # The DWARF file inside is named after the binary Cargo linked
        # (deps/deno-<hash>): `find` takes the directory's one file.
        return f"{name}.dSYM/Contents/Resources/DWARF"
    if is_windows(target):
        return name.rsplit(".", 1)[0] + ".pdb"
    if name.endswith(".so"):
        return name[: -len(".so")] + ".dwp"
    return f"{name}.dwp"


def archives(version, binary, target):
    """(binary archive, symbol archive) names of `binary` on `target`."""
    if binary == "libdenort":
        ext = ".zip" if is_windows(target) else ".tar.gz"
        return (
            f"deno-desktop-runtime-{version}-{target}-webview{ext}",
            f"deno-desktop-runtime-symbols-{version}-{target}.tar.gz",
        )
    kind = "cli" if binary == "deno" else "denort"
    symbols = "deno"
    if binary == "denort" and separate_denort_symbols(version):
        symbols = "denort"
    return (
        f"deno-{kind}-{version}-{target}.zip",
        f"{symbols}-symbols-{version}-{target}.tar.gz",
    )


def symbolizer_command(llvm_symbolizer, target, binary_path, debug_path, addresses):
    """The llvm-symbolizer command line for `addresses` (image-relative)."""
    cmd = [llvm_symbolizer, "--functions=linkage", "--inlining", "--demangle"]
    if is_macos(target):
        cmd.append(f"--obj={debug_path}")
        cmd += [hex(MACOS_TEXT_VMADDR + a) for a in addresses]
    elif is_windows(target):
        cmd += [f"--obj={binary_path}", "--relative-address"]
        cmd += [hex(a) for a in addresses]
    else:
        cmd += [f"--obj={binary_path}", f"--dwp={debug_path}"]
        cmd += [hex(a) for a in addresses]
    return cmd


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def checksums(path):
    """{file name: sha256} from a SHA256SUMS-style file."""
    sums = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if len(parts) == 2:
                sums[parts[1].lstrip("*")] = parts[0]
    return sums


def download(version, name, dest_dir):
    url = f"https://github.com/{REPO}/releases/download/{tag(version)}/{name}"
    dest = os.path.join(dest_dir, name)
    req = urllib.request.Request(url, headers={"User-Agent": "denext-symbolicate"})
    with urllib.request.urlopen(req, timeout=120) as res, open(dest, "wb") as out:
        shutil.copyfileobj(res, out)
    return dest


def fetch(version, name, sums_name, work, from_dir):
    """`name` from `from_dir` or the release, checked against `sums_name`."""
    def get(n):
        if from_dir:
            path = os.path.join(from_dir, n)
            if not os.path.exists(path):
                raise SystemExit(f"error: {n} is not in {from_dir}")
            return path
        return download(version, n, work)

    path = get(name)
    want = checksums(get(sums_name)).get(name)
    if want is None:
        raise SystemExit(f"error: {sums_name} does not list {name}")
    got = sha256_of(path)
    if got != want:
        raise SystemExit(f"error: {name} has SHA-256 {got}, {sums_name} says {want}")
    return path


def unpack(path, dest):
    if path.endswith(".zip"):
        with zipfile.ZipFile(path) as z:
            z.extractall(dest)
    else:
        with tarfile.open(path) as t:
            try:
                t.extractall(dest, filter="data")
            except TypeError:  # Python without extraction filters
                t.extractall(dest)


def find(root, rel):
    """`rel` under `root` (a directory: its first file), or a file of the
    same name anywhere below it."""
    direct = os.path.join(root, rel)
    if os.path.isdir(direct):
        files = sorted(f for f in os.listdir(direct) if os.path.isfile(os.path.join(direct, f)))
        return os.path.join(direct, files[0]) if files else None
    if os.path.exists(direct):
        return direct
    base = os.path.basename(rel)
    for dirpath, _dirs, files in os.walk(root):
        if base in files:
            return os.path.join(dirpath, base)
    return None


def release_assets(version):
    """{name: size} of a release, from the GitHub API."""
    url = f"https://api.github.com/repos/{REPO}/releases/tags/{tag(version)}"
    req = urllib.request.Request(url, headers={"User-Agent": "denext-symbolicate"})
    with urllib.request.urlopen(req, timeout=60) as res:
        return {a["name"]: a["size"] for a in json.load(res)["assets"]}


def main_check_release(argv):
    p = argparse.ArgumentParser(prog="symbolicate.py check-release")
    p.add_argument("version")
    p.add_argument("--assets", help="`gh release view --json assets` output")
    p.add_argument("--dir", help="a directory of the release's files (before publishing)")
    args = p.parse_args(argv)
    if args.dir:
        assets = {
            n: os.path.getsize(os.path.join(args.dir, n))
            for n in os.listdir(args.dir)
            if os.path.isfile(os.path.join(args.dir, n))
        }
    elif args.assets:
        with open(args.assets, encoding="utf-8") as f:
            data = json.load(f)
        assets = {a["name"]: a["size"] for a in data.get("assets", data)}
    else:
        assets = release_assets(args.version)
    problems = check_release(args.version, assets)
    for line in problems:
        print(line)
    if not problems:
        print(f"{tag(args.version)}: every symbol archive is there")
    return 1 if problems else 0


def main(argv):
    if argv and argv[0] == "check-release":
        return main_check_release(argv[1:])
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("version")
    p.add_argument("target", choices=TARGETS)
    p.add_argument("trace")
    p.add_argument("--binary", default="deno", choices=("deno", "denort", "libdenort"))
    p.add_argument("--from", dest="from_dir", help="a directory holding the archives")
    p.add_argument("--llvm-symbolizer", default=os.environ.get("LLVM_SYMBOLIZER", "llvm-symbolizer"))
    p.add_argument("--addresses", action="store_true", help="print the decoded addresses only")
    args = p.parse_args(argv)

    addresses = vlq_decode(args.trace)
    if args.addresses:
        for a in addresses:
            print(hex(a))
        return 0

    with tempfile.TemporaryDirectory() as work:
        bin_archive, sym_archive = archives(args.version, args.binary, args.target)
        bin_path = fetch(args.version, bin_archive, "SHA256SUMS", work, args.from_dir)
        sym_path = fetch(args.version, sym_archive, "SYMBOLS.SHA256SUMS", work, args.from_dir)
        out = os.path.join(work, "out")
        unpack(bin_path, out)
        unpack(sym_path, out)
        binary = find(out, binary_file(args.binary, args.target))
        debug = find(out, debug_file(args.binary, args.target))
        if binary is None or debug is None:
            raise SystemExit(
                f"error: {binary_file(args.binary, args.target)} or its debug file "
                f"({debug_file(args.binary, args.target)}) is not in the archives"
            )
        cmd = symbolizer_command(args.llvm_symbolizer, args.target, binary, debug, addresses)
        return subprocess.run(cmd, check=False).returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
