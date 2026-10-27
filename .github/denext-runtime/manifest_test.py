#!/usr/bin/env python3
"""Tests for manifest.py: each CLI archive's entry records the esbuild, both
native TypeScript compilers (`typescript-check/`, `typescript-lsp/`) and the
tsgolint (`tsgolint/`) it ships, recomputed from the zip, and an archive whose
files disagree with its BUILD_INFO.json is refused.

Usage: python3 -I .github/denext-runtime/manifest_test.py
"""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = os.path.join(HERE, "manifest.py")
VERSION = "3.0.0-rc.9"
TARGET = "x86_64-unknown-linux-gnu"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def tree_sha256(files):
    tree = hashlib.sha256()
    for rel in sorted(files):
        tree.update(f"{rel}\0{sha256(files[rel])}\n".encode())
    return tree.hexdigest()


def compiler(version):
    """A stand-in `typescript-<role>/` directory (but its typescript.json)."""
    return {
        "tsc": f"tsc {version}".encode(),
        "LICENSE": b"Apache-2.0",
        "NOTICE.txt": b"notice",
        "lib.es5.d.ts": f"// lib {version}".encode(),
    }


def linter(version):
    """A stand-in `tsgolint/` directory (but its tsgolint.json)."""
    return {
        "tsgolint": f"tsgolint {version}".encode(),
        "LICENSE": b"MIT",
    }


def write_cli_zip(dist, check, lsp, info_overrides=None, tsgolint=None):
    esbuild = b"esbuild binary"
    tsgolint = linter("7.0.2004") if tsgolint is None else tsgolint
    info = {
        "esbuild": {"version": "0.25.5", "sha256": sha256(esbuild)},
        "typescript_check": {"role": "check", "version": "7.0.2", "sha256": sha256(check["tsc"])},
        "typescript_lsp": {"role": "lsp", "version": "7.1.0-dev.1", "sha256": sha256(lsp["tsc"])},
        "tsgolint": {"version": "7.0.2004", "sha256": sha256(tsgolint.get("tsgolint", b""))},
    }
    info.update(info_overrides or {})
    name = f"deno-cli-{VERSION}-{TARGET}.zip"
    with zipfile.ZipFile(os.path.join(dist, name), "w") as z:
        z.writestr("deno", b"deno binary")
        z.writestr("esbuild", esbuild)
        z.writestr("esbuild.LICENSE", b"MIT")
        z.writestr("BUILD_INFO.json", json.dumps(info))
        for role, files in (("check", check), ("lsp", lsp)):
            z.writestr(f"typescript-{role}/typescript.json", json.dumps(info[f"typescript_{role}"]))
            for rel, data in files.items():
                z.writestr(f"typescript-{role}/{rel}", data)
        z.writestr("tsgolint/tsgolint.json", json.dumps(info["tsgolint"]))
        for rel, data in tsgolint.items():
            z.writestr(f"tsgolint/{rel}", data)
    return name


def run_manifest(dist, glibc=None):
    env = {
        "PATH": os.environ.get("PATH", ""),
        "VERSION": VERSION,
        "TAG": "",
        "REPOSITORY": "Brainwires/deno",
        "DENO_VERSION": "2.9.7",
        "DENO_SHA": "0" * 40,
        "LAUFEY_SHA": "1" * 40,
        "CEF_VERSION": "150.0.21",
        "CLI_ONLY": "1",
    }
    if glibc is not None:
        env["GLIBC_CEILING"] = glibc
    return subprocess.run(
        [sys.executable, "-I", MANIFEST, dist],
        env=env,
        capture_output=True,
        text=True,
    )


class ManifestTest(unittest.TestCase):
    def test_cli_targets_record_tsgolint(self):
        check, lsp, tsgolint = compiler("7.0.2"), compiler("7.1.0-dev.1"), linter("7.0.2004")
        with tempfile.TemporaryDirectory() as dist:
            write_cli_zip(dist, check, lsp, tsgolint=tsgolint)
            result = run_manifest(dist)
            self.assertEqual(result.returncode, 0, result.stderr)
            with open(os.path.join(dist, "manifest.json")) as f:
                manifest = json.load(f)
        self.assertEqual(
            manifest["cli"]["targets"][TARGET]["tsgolint"],
            {"version": "7.0.2004", "sha256": sha256(tsgolint["tsgolint"]), "tree_sha256": tree_sha256(tsgolint)},
        )

    def test_tsgolint_that_disagrees_with_build_info_is_refused(self):
        check, lsp = compiler("7.0.2"), compiler("7.1.0-dev.1")
        with tempfile.TemporaryDirectory() as dist:
            write_cli_zip(
                dist,
                check,
                lsp,
                {"tsgolint": {"version": "7.0.2004", "sha256": sha256(b"another tsgolint")}},
            )
            result = run_manifest(dist)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tsgolint/tsgolint sha256", result.stderr)

    def test_archive_without_tsgolint_is_refused(self):
        check, lsp = compiler("7.0.2"), compiler("7.1.0-dev.1")
        tsgolint = linter("7.0.2004")
        del tsgolint["tsgolint"]
        with tempfile.TemporaryDirectory() as dist:
            write_cli_zip(dist, check, lsp, tsgolint=tsgolint)
            result = run_manifest(dist)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("has no tsgolint/tsgolint", result.stderr)

    def test_cli_targets_record_both_compilers(self):
        check, lsp = compiler("7.0.2"), compiler("7.1.0-dev.1")
        with tempfile.TemporaryDirectory() as dist:
            write_cli_zip(dist, check, lsp)
            result = run_manifest(dist)
            self.assertEqual(result.returncode, 0, result.stderr)
            with open(os.path.join(dist, "manifest.json")) as f:
                manifest = json.load(f)
        entry = manifest["cli"]["targets"][TARGET]
        self.assertEqual(
            entry["typescript_check"],
            {"version": "7.0.2", "sha256": sha256(check["tsc"]), "tree_sha256": tree_sha256(check)},
        )
        self.assertEqual(
            entry["typescript_lsp"],
            {"version": "7.1.0-dev.1", "sha256": sha256(lsp["tsc"]), "tree_sha256": tree_sha256(lsp)},
        )
        self.assertNotEqual(entry["typescript_lsp"]["tree_sha256"], entry["typescript_check"]["tree_sha256"])

    def test_cli_records_the_identity_marker(self):
        check, lsp = compiler("7.0.2"), compiler("7.1.0-dev.1")
        with tempfile.TemporaryDirectory() as dist:
            write_cli_zip(dist, check, lsp)
            result = run_manifest(dist)
            self.assertEqual(result.returncode, 0, result.stderr)
            with open(os.path.join(dist, "manifest.json")) as f:
                manifest = json.load(f)
        self.assertEqual(
            manifest["cli"]["identity"],
            {"buildDenext": VERSION, "versionLine": f"denext {VERSION} (Brainwires)"},
        )

    def test_cli_records_the_glibc_ceiling(self):
        check, lsp = compiler("7.0.2"), compiler("7.1.0-dev.1")
        for glibc, want in (("2.27", "2.27"), (None, None), ("", None)):
            with tempfile.TemporaryDirectory() as dist:
                write_cli_zip(dist, check, lsp)
                result = run_manifest(dist, glibc)
                self.assertEqual(result.returncode, 0, result.stderr)
                with open(os.path.join(dist, "manifest.json")) as f:
                    manifest = json.load(f)
            self.assertIn("glibc", manifest["cli"])
            self.assertEqual(manifest["cli"]["glibc"], want)

    def test_a_malformed_glibc_ceiling_is_refused(self):
        check, lsp = compiler("7.0.2"), compiler("7.1.0-dev.1")
        for glibc in ("2", "2.27.1", "GLIBC_2.27", "latest"):
            with tempfile.TemporaryDirectory() as dist:
                write_cli_zip(dist, check, lsp)
                result = run_manifest(dist, glibc)
            self.assertNotEqual(result.returncode, 0, glibc)
            self.assertIn("is not a glibc version", result.stderr)

    def test_lsp_binary_that_disagrees_with_build_info_is_refused(self):
        check, lsp = compiler("7.0.2"), compiler("7.1.0-dev.1")
        with tempfile.TemporaryDirectory() as dist:
            write_cli_zip(
                dist,
                check,
                lsp,
                {"typescript_lsp": {"role": "lsp", "version": "7.1.0-dev.1", "sha256": sha256(b"another tsc")}},
            )
            result = run_manifest(dist)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("typescript-lsp/tsc sha256", result.stderr)

    def test_archive_without_the_lsp_compiler_is_refused(self):
        check, lsp = compiler("7.0.2"), compiler("7.1.0-dev.1")
        del lsp["LICENSE"]
        with tempfile.TemporaryDirectory() as dist:
            write_cli_zip(dist, check, lsp)
            result = run_manifest(dist)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("has no typescript-lsp/LICENSE", result.stderr)


if __name__ == "__main__":
    unittest.main()
