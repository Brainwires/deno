#!/usr/bin/env python3
"""Tests of denort_pins.py. Run: python3 -I .github/denext-runtime/denort_pins_test.py"""
import hashlib
import os
import re
import sys
import tempfile
import unittest
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import denort_pins as pins  # noqa: E402

V = "3.0.0-denext.8"
TARGETS = sorted(pins.RELEASE_TARGETS)


def full_list():
    return ",".join(f"{t}={'a' * 64}" for t in TARGETS)


class CheckList(unittest.TestCase):
    def test_exact_set(self):
        pins.check_list(full_list())

    def test_six_entries_with_a_wrong_name_are_refused(self):
        # What counting archives accepted.
        wrong = full_list().replace("x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl")
        with self.assertRaisesRegex(pins.PinError, "linux-musl"):
            pins.check_list(wrong)

    def test_six_entries_with_a_duplicate_are_refused(self):
        wrong = full_list().replace("aarch64-pc-windows-msvc", "aarch64-apple-darwin")
        with self.assertRaisesRegex(pins.PinError, "duplicated"):
            pins.check_list(wrong)

    def test_fewer_and_empty_are_refused(self):
        for text in ("", ",".join(full_list().split(",")[:5])):
            with self.assertRaisesRegex(pins.PinError, "missing"):
                pins.check_list(text)

    def test_malformed_hash_is_refused(self):
        with self.assertRaisesRegex(pins.PinError, "malformed"):
            pins.check_list(f"{TARGETS[0]}=abc")


class CheckCli(unittest.TestCase):
    def build(self, dist, targets=TARGETS, carried=None, skip=()):
        """CLI and denort archives; `carried` is what each CLI binary holds."""
        for t in targets:
            with zipfile.ZipFile(os.path.join(dist, f"deno-denort-{V}-{t}.zip"), "w") as z:
                z.writestr("denort", f"denort {t}")
        entries = []
        for t in targets:
            with open(os.path.join(dist, f"deno-denort-{V}-{t}.zip"), "rb") as f:
                entries.append(f"{t}={hashlib.sha256(f.read()).hexdigest()}")
        text = ",".join(entries) if carried is None else carried
        for t in targets:
            if t in skip:
                continue
            binary = "deno.exe" if t.endswith("windows-msvc") else "deno"
            with zipfile.ZipFile(os.path.join(dist, f"deno-cli-{V}-{t}.zip"), "w") as z:
                z.writestr(binary, b"\x7fELF prefix " + text.encode() + b" suffix")

    def test_every_binary_carries_every_archive(self):
        with tempfile.TemporaryDirectory() as dist:
            self.build(dist)
            pins.check_cli(V, dist, release=True)

    def test_a_cli_built_without_the_list_is_refused(self):
        with tempfile.TemporaryDirectory() as dist:
            self.build(dist, carried="")
            with self.assertRaisesRegex(pins.PinError, "does not carry"):
                pins.check_cli(V, dist, release=True)

    def test_a_list_of_other_hashes_is_refused(self):
        with tempfile.TemporaryDirectory() as dist:
            self.build(dist, carried=full_list())
            with self.assertRaisesRegex(pins.PinError, "does not carry"):
                pins.check_cli(V, dist, release=True)

    def test_a_replaced_denort_archive_is_refused(self):
        with tempfile.TemporaryDirectory() as dist:
            self.build(dist)
            with open(os.path.join(dist, f"deno-denort-{V}-{TARGETS[0]}.zip"), "ab") as f:
                f.write(b"x")
            with self.assertRaisesRegex(pins.PinError, TARGETS[0]):
                pins.check_cli(V, dist, release=True)

    def test_release_needs_the_exact_targets(self):
        with tempfile.TemporaryDirectory() as dist:
            self.build(dist, skip=(TARGETS[0],))
            pins.check_cli(V, dist)  # a filtered dispatch
            with self.assertRaisesRegex(pins.PinError, "CLI archives: missing"):
                pins.check_cli(V, dist, release=True)

    def test_no_archives_is_refused(self):
        with tempfile.TemporaryDirectory() as dist:
            with self.assertRaisesRegex(pins.PinError, "no deno-cli"):
                pins.check_cli(V, dist)


class CheckManifest(unittest.TestCase):
    def manifest(self, targets=TARGETS):
        return {
            "targets": {t: {"runtimeLib": "x", "webview": {}, "cef": {}} for t in targets},
            "cli": {"targets": {t: {} for t in targets}},
            "denort": {"targets": {t: {} for t in targets}},
        }

    def test_exact(self):
        pins.check_manifest(self.manifest())

    def test_wrong_name_in_each_section_is_refused(self):
        for section in ("cli", "denort"):
            m = self.manifest()
            m[section]["targets"].pop(TARGETS[0])
            m[section]["targets"]["x86_64-unknown-linux-musl"] = {}
            with self.assertRaisesRegex(pins.PinError, "linux-musl"):
                pins.check_manifest(m)
        m = self.manifest()
        m["targets"].pop(TARGETS[0])
        m["targets"]["x86_64-unknown-linux-musl"] = {"webview": {}, "cef": {}}
        with self.assertRaisesRegex(pins.PinError, "linux-musl"):
            pins.check_manifest(m)

    def test_missing_backend_is_refused(self):
        m = self.manifest()
        del m["targets"][TARGETS[0]]["cef"]
        with self.assertRaisesRegex(pins.PinError, "backends"):
            pins.check_manifest(m)


class Workflow(unittest.TestCase):
    def test_the_workflow_builds_the_release_targets(self):
        path = os.path.join(HERE, "..", "workflows", "denext_runtime.yml")
        with open(path) as f:
            built = re.findall(r'\{"target": "([^"]+)", "os":', f.read())
        self.assertEqual(sorted(built), TARGETS)


if __name__ == "__main__":
    unittest.main()
