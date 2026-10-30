#!/usr/bin/env python3
"""Tests for audit_ignores.py, the check beside denext_tests.yml's cargo
audit that an ignored advisory has not become reachable: RUSTSEC-2026-0418's
ignore fails once cc 1.6.0 or later is in Cargo.lock, and passes while cc is
older or the ignore is gone. The last cases run the script on files.

Usage: python3 -I .github/denext-runtime/audit_ignores_test.py
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "audit_ignores.py")

spec = importlib.util.spec_from_file_location("audit_ignores", SCRIPT)
ai = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ai)

AUDIT = """\
[advisories]
ignore = [
  # rsa: accepted risk.
  "RUSTSEC-2023-0071",
  # libz-sys: unreachable while cc < 1.6 ("RUSTSEC-2099-0001" in a comment).
  "RUSTSEC-2026-0418",
]
"""
AUDIT_WITHOUT = """\
[advisories]
ignore = [
  "RUSTSEC-2023-0071",
  # "RUSTSEC-2026-0418",
]
"""


def lock(*cc_versions):
    blocks = ['[[package]]\nname = "ccx"\nversion = "9.0.0"\n']
    for v in cc_versions:
        blocks.append(f'[[package]]\nname = "cc"\nversion = "{v}"\nsource = "registry"\n')
    return "\n".join(blocks)


class Parse(unittest.TestCase):
    def test_ignored_advisories_leave_comments_out(self):
        self.assertEqual(ai.ignored_advisories(AUDIT), {"RUSTSEC-2023-0071", "RUSTSEC-2026-0418"})
        self.assertEqual(ai.ignored_advisories(AUDIT_WITHOUT), {"RUSTSEC-2023-0071"})
        self.assertEqual(ai.ignored_advisories("[advisories]\n"), set())

    def test_lock_versions_match_the_name_exactly(self):
        self.assertEqual(ai.lock_versions(lock("1.2.56"), "cc"), ["1.2.56"])
        self.assertEqual(ai.lock_versions(lock(), "cc"), [])

    def test_version_key(self):
        self.assertEqual(ai.version_key("1.6.0"), (1, 6, 0))
        self.assertEqual(ai.version_key("1.10.0-rc.1"), (1, 10, 0))
        self.assertGreater(ai.version_key("1.10.0"), ai.version_key("1.6.0"))
        self.assertLess(ai.version_key("1.2.56"), (1, 6, 0))


class Check(unittest.TestCase):
    def test_cc_below_1_6_passes(self):
        self.assertEqual(ai.check(AUDIT, lock("1.2.56")), [])
        self.assertEqual(ai.check(AUDIT, lock("1.5.99")), [])

    def test_cc_1_6_or_later_fails(self):
        for v in ("1.6.0", "1.6.2", "1.10.0", "2.0.0"):
            errors = ai.check(AUDIT, lock("1.2.56", v))
            self.assertEqual(len(errors), 1, v)
            self.assertIn("RUSTSEC-2026-0418", errors[0])
            self.assertIn(f"cc {v}", errors[0])
            self.assertTrue(errors[0].startswith("::error::"))

    def test_no_ignore_no_failure(self):
        self.assertEqual(ai.check(AUDIT_WITHOUT, lock("1.6.0")), [])


class Script(unittest.TestCase):
    def run_script(self, audit, lock_text):
        with tempfile.TemporaryDirectory() as d:
            a = os.path.join(d, "audit.toml")
            lk = os.path.join(d, "Cargo.lock")
            with open(a, "w") as f:
                f.write(audit)
            with open(lk, "w") as f:
                f.write(lock_text)
            return subprocess.run(
                [sys.executable, "-I", SCRIPT, "--audit-toml", a, "--lock", lk],
                capture_output=True,
                text=True,
            )

    def test_exit_codes(self):
        ok = self.run_script(AUDIT, lock("1.2.56"))
        self.assertEqual(ok.returncode, 0, ok.stdout + ok.stderr)
        bad = self.run_script(AUDIT, lock("1.6.0"))
        self.assertEqual(bad.returncode, 1, bad.stdout + bad.stderr)
        self.assertIn("::error::", bad.stdout)

    def test_the_repository_passes(self):
        root = os.path.dirname(os.path.dirname(HERE))
        r = subprocess.run(
            [sys.executable, "-I", SCRIPT,
             "--audit-toml", os.path.join(root, ".cargo", "audit.toml"),
             "--lock", os.path.join(root, "Cargo.lock")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main()
