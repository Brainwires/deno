#!/usr/bin/env python3
"""Tests for engine_staleness.py, denext_runtime.yml's engine staleness gate:
a V8 (Cargo.lock's v8 crate) or CEF (laufey's CEF_FULL_VERSION) pin more than
one Chrome stable major behind, and not on Chrome's Extended Stable milestone,
warns, and fails a release tag; one on Extended Stable only warns, saying it
is still patched; a Chrome stable lookup that fails warns and passes. The last cases run the script against a
canned versionhistory answer (file://) and a closed port.

Usage: python3 -I .github/denext-runtime/engine_staleness_test.py
"""

import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
GATE = os.path.join(HERE, "engine_staleness.py")

spec = importlib.util.spec_from_file_location("engine_staleness", GATE)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

LOCK = """\
[[package]]
name = "v8"
version = "152.2.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "v8x"
version = "149.4.0-rc.4"
"""
CEF = "150.0.21+ga61e9a5+chromium-150.0.7871.255"


class Parse(unittest.TestCase):
    def test_reads_the_v8_crate_not_v8x(self):
        self.assertEqual(gate.v8_crate_version(LOCK), "152.2.0")

    def test_no_v8_crate(self):
        self.assertIsNone(gate.v8_crate_version('[[package]]\nname = "v8x"\nversion = "1.0.0"\n'))

    def test_crate_major_is_the_milestone(self):
        self.assertEqual(gate.v8_milestone("152.2.0"), (152, "15.2"))
        self.assertEqual(gate.v8_milestone("150.4.0"), (150, "15.0"))
        self.assertEqual(gate.v8_milestone("149.4.0"), (149, "14.9"))
        self.assertIsNone(gate.v8_milestone("nope"))

    def test_cef_milestone(self):
        self.assertEqual(gate.cef_milestone(CEF), 150)
        self.assertEqual(gate.cef_milestone("151.0.3"), 151)
        self.assertIsNone(gate.cef_milestone("abc"))
        self.assertIsNone(gate.cef_milestone(""))

    def test_stable_is_the_newest_answer(self):
        body = json.dumps({"versions": [{"version": "155.0.1.2"}, {"version": "156.0.8078.73"}]})
        self.assertEqual(gate.stable_from_answer(body), 156)

    def test_an_empty_answer_is_unreadable(self):
        with self.assertRaises(ValueError):
            gate.stable_from_answer('{"versions": []}')


class Evaluate(unittest.TestCase):
    def test_one_behind_is_allowed(self):
        code, lines = gate.evaluate([("V8", 155)], 156, tag=True, enforce="tag")
        self.assertEqual(code, 0)
        self.assertNotIn("::", lines[0])

    def test_ahead_of_stable_is_allowed(self):
        # A beta-channel engine.
        code, _ = gate.evaluate([("V8", 157)], 156, tag=True, enforce="tag")
        self.assertEqual(code, 0)

    def test_two_behind_warns_on_a_branch(self):
        code, lines = gate.evaluate([("V8", 154)], 156, tag=False, enforce="tag")
        self.assertEqual(code, 0)
        self.assertTrue(lines[0].startswith("::warning::V8 is 2 Chrome stable majors behind"))

    def test_two_behind_fails_a_tag(self):
        code, lines = gate.evaluate([("V8", 155), ("CEF", 150)], 156, tag=True, enforce="tag")
        self.assertEqual(code, 1)
        self.assertNotIn("::", lines[0])
        self.assertTrue(lines[1].startswith("::error::CEF is 6 Chrome stable majors behind"))

    def test_on_extended_stable_warns_but_never_fails_a_tag(self):
        # V8 152 with stable 154 and Extended Stable 152: still patched.
        code, lines = gate.evaluate([("V8", 152)], 154, tag=True, enforce="tag", extended=152)
        self.assertEqual(code, 0)
        self.assertTrue(lines[0].startswith("::warning::V8 is 2 Chrome stable majors behind"))
        self.assertIn("on Chrome's Extended Stable milestone 152", lines[0])
        self.assertIn("still gets upstream security fixes", lines[0])
        self.assertNotIn("no longer receives", lines[0])

    def test_behind_extended_stable_is_stale(self):
        code, lines = gate.evaluate([("V8", 152)], 156, tag=True, enforce="tag", extended=156)
        self.assertEqual(code, 1)
        self.assertTrue(lines[0].startswith("::error::V8 is 4 Chrome stable majors behind"))
        self.assertIn("Extended Stable 156", lines[0])
        self.assertIn("no longer receives upstream security fixes", lines[0])

    def test_extended_stable_unknown_judges_by_stable(self):
        code, lines = gate.evaluate([("V8", 152)], 154, tag=True, enforce="tag", extended=None)
        self.assertEqual(code, 1)
        self.assertNotIn("Extended Stable", lines[0])

    def test_warn_mode_keeps_a_tag_at_a_warning(self):
        code, lines = gate.evaluate([("V8", 150)], 156, tag=True, enforce="warn")
        self.assertEqual(code, 0)
        self.assertTrue(lines[0].startswith("::warning::"))

    def test_an_engines_own_mode_overrides_enforce(self):
        code, lines = gate.evaluate(
            [("V8", 150, "tag"), ("CEF", 150, "warn")], 156, tag=True, enforce="warn"
        )
        self.assertEqual(code, 1)
        self.assertTrue(lines[0].startswith("::error::V8"))
        self.assertTrue(lines[1].startswith("::warning::CEF"))


class Script(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.lock = os.path.join(self.dir.name, "Cargo.lock")
        pathlib.Path(self.lock).write_text(LOCK)

    def answer(self, *versions, name="answer.json"):
        path = os.path.join(self.dir.name, name)
        pathlib.Path(path).write_text(
            json.dumps({"versions": [{"version": v} for v in versions]})
        )
        return pathlib.Path(path).as_uri()

    def gate(
        self, *args, url=None, enforce=None, extended_url="http://127.0.0.1:9/versions", **modes
    ):
        env = dict(os.environ)
        for var in ("ENGINE_STALENESS_ENFORCE", "ENGINE_STALENESS_ENFORCE_V8", "ENGINE_STALENESS_ENFORCE_CEF"):
            env.pop(var, None)
        if url is not None:
            env["CHROME_STABLE_URL"] = url
        # A closed port unless a case gives an answer: never the network.
        env["CHROME_EXTENDED_URL"] = extended_url
        if enforce is not None:
            env["ENGINE_STALENESS_ENFORCE"] = enforce
        for engine, mode in modes.items():
            env[f"ENGINE_STALENESS_ENFORCE_{engine.upper()}"] = mode
        return subprocess.run(
            [sys.executable, "-I", GATE, "--lock", self.lock, *args],
            env=env,
            capture_output=True,
            text=True,
        )

    def test_current_pins_pass_a_tag(self):
        r = self.gate("--tag", "--cef", "152.0.1+gabc+chromium-152.0.1.2", url=self.answer("153.0.1.2"))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("V8 15.2 (v8 crate 152.2.0) is current enough", r.stdout)
        self.assertNotIn("::", r.stdout)

    def test_stale_pins_fail_a_tag(self):
        r = self.gate("--tag", "--cef", CEF, url=self.answer("156.0.8078.73"))
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::V8 15.2 (v8 crate 152.2.0) is 4 Chrome stable majors behind", r.stdout)
        self.assertIn(f"::error::CEF {CEF} is 6 Chrome stable majors behind", r.stdout)

    def test_stale_pins_only_warn_on_a_branch(self):
        r = self.gate("--cef", CEF, url=self.answer("156.0.8078.73"))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(r.stdout.count("::warning::"), 2)

    def test_warn_mode_passes_a_stale_tag(self):
        r = self.gate("--tag", "--cef", CEF, url=self.answer("156.0.8078.73"), enforce="warn")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(r.stdout.count("::warning::"), 2)

    def test_cef_warn_keeps_a_stale_cef_at_a_warning_while_v8_is_enforced(self):
        # denext_runtime.yml: V8 is `tag`, CEF `warn` (DENEXT.md item 184).
        r = self.gate("--tag", "--cef", CEF, url=self.answer("156.0.8078.73"), enforce="tag", cef="warn")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::V8 15.2", r.stdout)
        self.assertIn(f"::warning::CEF {CEF}", r.stdout)

    def test_a_current_v8_and_a_stale_cef_at_warn_pass_a_tag(self):
        pathlib.Path(self.lock).write_text(LOCK.replace('"152.2.0"', '"156.0.0+denext.1"'))
        r = self.gate("--tag", "--cef", CEF, url=self.answer("156.0.8078.73"), enforce="tag", cef="warn")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("V8 15.6 (v8 crate 156.0.0+denext.1) is current enough", r.stdout)
        self.assertIn(f"::warning::CEF {CEF}", r.stdout)

    def test_an_unknown_engine_mode_fails(self):
        r = self.gate("--stable", "152", v8="sometimes")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::ENGINE_STALENESS_ENFORCE_V8 is 'sometimes'", r.stdout)

    def test_a_pin_on_extended_stable_passes_a_tag(self):
        r = self.gate(
            "--tag",
            url=self.answer("154.0.1.2"),
            extended_url=self.answer("152.0.7.8", name="extended.json"),
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("::warning::V8 15.2 (v8 crate 152.2.0) is 2 Chrome stable majors behind", r.stdout)
        self.assertIn("on Chrome's Extended Stable milestone 152", r.stdout)

    def test_extended_flag_skips_its_lookup(self):
        r = self.gate("--tag", "--stable", "154", "--extended", "152")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("could not learn the Chrome Extended Stable", r.stdout)
        self.assertIn("Extended Stable milestone 152", r.stdout)

    def test_extended_lookup_failure_is_a_notice(self):
        r = self.gate("--tag", "--stable", "154")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::notice::could not learn the Chrome Extended Stable milestone", r.stdout)
        self.assertIn("::error::V8 15.2 (v8 crate 152.2.0) is 2 Chrome stable majors behind", r.stdout)

    def test_an_unknown_mode_fails(self):
        r = self.gate("--stable", "152", enforce="always")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::ENGINE_STALENESS_ENFORCE", r.stdout)

    def test_offline_warns_and_passes_a_tag(self):
        # Port 9 (discard) is closed: the lookup fails to connect.
        r = self.gate("--tag", "--cef", CEF, url="http://127.0.0.1:9/versions")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("::warning::could not learn the Chrome stable milestone", r.stdout)

    def test_an_unreadable_answer_warns_and_passes(self):
        path = os.path.join(self.dir.name, "garbage.json")
        pathlib.Path(path).write_text("<html>rate limited</html>")
        r = self.gate("--tag", url=pathlib.Path(path).as_uri())
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("::warning::could not learn", r.stdout)

    def test_stable_flag_skips_the_lookup(self):
        r = self.gate("--tag", "--stable", "153", url="http://127.0.0.1:9/versions")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("is current enough", r.stdout)

    def test_an_unreadable_cef_pin_fails(self):
        r = self.gate("--cef", "garbage", "--stable", "152")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::cannot read a Chrome milestone", r.stdout)

    def test_a_lock_without_v8_fails(self):
        pathlib.Path(self.lock).write_text('[[package]]\nname = "serde"\nversion = "1.0.0"\n')
        r = self.gate("--stable", "152")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::no readable v8 package version", r.stdout)


if __name__ == "__main__":
    unittest.main()
