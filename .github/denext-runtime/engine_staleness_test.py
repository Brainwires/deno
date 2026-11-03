#!/usr/bin/env python3
"""Tests for engine_staleness.py, denext_runtime.yml's engine staleness gate:
a V8 (Cargo.lock's v8 crate) or CEF (laufey's CEF_FULL_VERSION) pin more than
one Chrome stable major behind, and not on Chrome's Extended Stable milestone,
warns, and fails a release tag; one on Extended Stable only warns, saying it
is still patched; a Chrome stable lookup that fails warns and passes. A pin
whose patch level trails its branch's patches by more than 14 days (V8's
`Version` commits on its branch-heads, CEF's builds on its milestone) is
stale the same way; a patch level it cannot learn warns and passes. The
script cases run against canned answers (file://) and a closed port, never
the network.

Usage: python3 -I .github/denext-runtime/engine_staleness_test.py
"""

import datetime
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
# The fork's own V8: a git revision of Brainwires/rusty_v8's `crate` branch.
REV = "2ddc1c5de87f7931e698cd6a58eef52b02e49b85"
GIT_LOCK = f"""\
[[package]]
name = "v8"
version = "156.0.0+denext.3"
source = "git+https://github.com/Brainwires/rusty_v8?rev={REV}#{REV}"
"""
HEADER = """\
#define V8_MAJOR_VERSION 15
#define V8_MINOR_VERSION 6
#define V8_BUILD_NUMBER 75
#define V8_PATCH_LEVEL 33
"""
CEF156 = "156.0.15+gcf671c0+chromium-156.0.8078.80"
CLOSED = "http://127.0.0.1:9/x"


def utc(*args):
    return datetime.datetime(*args, tzinfo=datetime.timezone.utc)


def gitiles(text):
    return ")]}'\n" + text


def log_answer(*commits):
    """A gitiles +log answer of (subject, "Sat Oct 31 13:36:25 2026")."""
    return gitiles(json.dumps({
        "log": [
            {"commit": f"{i:040x}", "message": f"{subject}\n\nChange-Id: x\n", "committer": {"time": when}}
            for i, (subject, when) in enumerate(commits)
        ]
    }))


# V8's branch-heads/15.6 around 15.6.75.33 (2026-10-31), newest first.
LOG_15_6 = log_answer(
    ("Version 15.6.75.35", "Mon Nov 02 09:47:22 2026"),
    ("[M156] [fastapi] Don't catch uncatchable exceptions after deopt", "Mon Nov 02 09:45:19 2026"),
    ("Version 15.6.75.34", "Mon Nov 02 09:23:39 2026"),
    ("[M156] [modules] Do not cache star exports after linking", "Mon Nov 02 09:18:17 2026"),
    ("Version 15.6.75.33", "Sat Oct 31 13:36:25 2026"),
    ("Version 15.6.75.32", "Fri Oct 30 10:17:33 2026"),
)


def cef_build(version, *uploads):
    return {
        "cef_version": version,
        "channel": "stable",
        "chromium_version": version.split("chromium-", 1)[1],
        "files": [{"last_modified": u, "name": f"cef_binary_{version}_{i}"} for i, u in enumerate(uploads)],
    }


CEF_INDEX = json.dumps({
    "linux64": {
        "versions": [
            cef_build(CEF156, "2026-10-31T04:24:08.633000Z"),
            cef_build("150.0.23+g4ccc4ca+chromium-150.0.7871.287", "2026-10-31T05:23:06Z"),
            cef_build("154.0.34+g14c5a08+chromium-154.0.8037.98", "2026-10-05T04:07:59Z"),
            cef_build("150.0.22+gc1a097e+chromium-150.0.7871.256", "2026-10-09T04:31:37Z", "2026-10-09T04:30:00Z"),
            cef_build(CEF, "2026-09-23T04:25:41.193000Z"),
        ]
    },
    "macosarm64": {"versions": [cef_build("150.0.99+gzzz+chromium-150.0.1.1", "2026-12-01T00:00:00Z")]},
})


def milestone_lines(stdout):
    """The milestone check's lines (not the patch check's)."""
    return [line for line in stdout.splitlines() if "patch" not in line]


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


class Patch(unittest.TestCase):
    def test_reads_the_git_source_of_the_v8_crate(self):
        self.assertEqual(gate.v8_crate_source(GIT_LOCK), ("Brainwires/rusty_v8", REV))
        self.assertIsNone(gate.v8_crate_source(LOCK))  # a registry crate
        self.assertIsNone(gate.v8_crate_source('[[package]]\nname = "v8x"\nversion = "1.0.0"\n'))

    def test_reads_the_v8_version_header(self):
        self.assertEqual(gate.v8_version_from_header(HEADER), "15.6.75.33")
        self.assertIsNone(gate.v8_version_from_header(HEADER.replace("PATCH_LEVEL", "PATCH")))

    def test_gitiles_time(self):
        self.assertEqual(gate.gitiles_time("Sat Oct 31 13:36:25 2026"), utc(2026, 10, 31, 13, 36, 25))
        self.assertEqual(gate.gitiles_time("Sat Oct 31 13:36:25 2026 +0100"), utc(2026, 10, 31, 12, 36, 25))
        self.assertEqual(gate.gitiles_time("Mon Nov  2 09:23:39 2026"), utc(2026, 11, 2, 9, 23, 39))
        with self.assertRaises(ValueError):
            gate.gitiles_time("2026-10-31")

    def test_branch_versions_are_the_version_commits(self):
        self.assertEqual(
            gate.v8_branch_versions(LOG_15_6),
            [
                ("15.6.75.35", utc(2026, 11, 2, 9, 47, 22)),
                ("15.6.75.34", utc(2026, 11, 2, 9, 23, 39)),
                ("15.6.75.33", utc(2026, 10, 31, 13, 36, 25)),
                ("15.6.75.32", utc(2026, 10, 30, 10, 17, 33)),
            ],
        )

    def test_cef_branch_is_the_pins_milestone_newest_first(self):
        versions = gate.cef_branch_versions(CEF_INDEX, CEF)
        self.assertEqual([v for v, _ in versions], [
            "150.0.23+g4ccc4ca+chromium-150.0.7871.287",
            "150.0.22+gc1a097e+chromium-150.0.7871.256",
            CEF,
        ])
        # A build is dated by its earliest upload; other platforms are not read.
        self.assertEqual(versions[1][1], utc(2026, 10, 9, 4, 30))

    def test_no_newer_patch_is_no_lag(self):
        versions = gate.v8_branch_versions(LOG_15_6)
        self.assertIsNone(gate.patch_lag("15.6.75.35", utc(2026, 11, 2, 9, 47, 22), versions, utc(2027, 1, 1)))

    def test_lag_is_the_larger_of_behind_and_unpatched(self):
        versions = [("1.0.0.3", utc(2026, 10, 20)), ("1.0.0.2", utc(2026, 10, 2)), ("1.0.0.1", utc(2026, 10, 1))]
        lag = gate.patch_lag("1.0.0.1", utc(2026, 10, 1), versions, utc(2026, 10, 10))
        self.assertEqual(lag["newest"], "1.0.0.3")
        self.assertEqual(lag["behind_days"], 19)
        self.assertEqual(lag["unpatched_days"], 8)
        self.assertEqual(lag["lag_days"], 19)
        # One early fix left out for weeks, on a branch that has not moved since.
        lag = gate.patch_lag("1.0.0.1", utc(2026, 10, 1), versions[1:], utc(2026, 10, 30))
        self.assertEqual(lag["behind_days"], 1)
        self.assertEqual(lag["lag_days"], 28)

    def test_fourteen_days_is_allowed_and_more_is_stale(self):
        versions = [("1.0.0.2", utc(2026, 10, 15)), ("1.0.0.1", utc(2026, 10, 1))]
        state, line = gate.patch_verdict("V8 1.0.0.1", "1.0.0.1", gate.patch_lag("1.0.0.1", utc(2026, 10, 1), versions, utc(2026, 10, 15)))
        self.assertEqual(state, "current", line)
        self.assertIn("by at most 14 days", line)
        state, line = gate.patch_verdict("V8 1.0.0.1", "1.0.0.1", gate.patch_lag("1.0.0.1", utc(2026, 10, 1), versions, utc(2026, 10, 30)))
        self.assertEqual(state, "stale", line)
        self.assertIn("more than 14 days behind its branch's patches", line)
        self.assertIn("newest patch 1.0.0.2 is 14 days newer", line)
        self.assertIn("has been out 15 days", line)

    def test_on_the_newest_patch(self):
        self.assertEqual(gate.patch_verdict("V8 1.0.0.2", "1.0.0.2", None), ("current", "V8 1.0.0.2 is on its branch's newest patch"))

    def test_a_stale_patch_fails_a_tag_in_tag_mode(self):
        patch = ("stale", "V8 1.0.0.1 is more than 14 days behind")
        code, lines = gate.evaluate([("V8", 156, "tag", patch)], 156, tag=True, enforce="tag")
        self.assertEqual(code, 1)
        self.assertNotIn("::", lines[0])
        self.assertEqual(lines[1], "::error::V8 1.0.0.1 is more than 14 days behind")

    def test_a_stale_patch_only_warns_on_a_branch_or_in_warn_mode(self):
        patch = ("stale", "V8 1.0.0.1 is more than 14 days behind")
        for tag, mode in ((False, "tag"), (True, "warn")):
            code, lines = gate.evaluate([("V8", 156, mode, patch)], 156, tag=tag, enforce="tag")
            self.assertEqual(code, 0)
            self.assertTrue(lines[1].startswith("::warning::"))

    def test_an_unknown_patch_level_warns_and_never_fails(self):
        patch = ("unknown", "could not learn the patch level of V8")
        code, lines = gate.evaluate([("V8", 156, "tag", patch)], 156, tag=True, enforce="tag")
        self.assertEqual(code, 0)
        self.assertEqual(lines[1], "::warning::could not learn the patch level of V8")

    def test_an_unknown_stable_still_runs_the_patch_check(self):
        patch = ("stale", "V8 1.0.0.1 is more than 14 days behind")
        code, lines = gate.evaluate([("V8", 150, "tag", patch)], None, tag=True, enforce="tag")
        self.assertEqual(code, 1)
        self.assertEqual(lines, ["::error::V8 1.0.0.1 is more than 14 days behind"])


class Script(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.lock = os.path.join(self.dir.name, "Cargo.lock")
        pathlib.Path(self.lock).write_text(LOCK)
        self.patch_urls = {}

    def file(self, name, text):
        path = pathlib.Path(self.dir.name, name)
        path.write_text(text)
        return path.as_uri()

    def answers(self, log=LOG_15_6, index=CEF_INDEX):
        """Canned patch-level sources for the fork's git-sourced V8."""
        pathlib.Path(self.lock).write_text(GIT_LOCK)
        self.file(f"header-{REV}.h", HEADER)
        self.file("log-15.6.json", log)
        self.file("index.json", index)
        base = pathlib.Path(self.dir.name).as_uri()
        self.patch_urls = {
            "V8_PIN_HEADER_URL": base + "/header-{rev}.h",
            "V8_BRANCH_LOG_URL": base + "/log-{branch}.json",
            "V8_TAG_URL": base + "/tag-{version}.json",
            "CEF_INDEX_URL": base + "/index.json",
        }

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
        # The patch-level sources: closed ports unless a case gives answers.
        for var in ("V8_PIN_HEADER_URL", "V8_BRANCH_LOG_URL", "V8_TAG_URL", "CEF_INDEX_URL"):
            env[var] = self.patch_urls.get(var, CLOSED)
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
        self.assertEqual([line for line in milestone_lines(r.stdout) if "::" in line], [])
        # The registry crate's and the offline CEF pin's patch levels are unknown.
        self.assertEqual(r.stdout.count("::warning::could not learn the patch level"), 2)

    def test_stale_pins_fail_a_tag(self):
        r = self.gate("--tag", "--cef", CEF, url=self.answer("156.0.8078.73"))
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::V8 15.2 (v8 crate 152.2.0) is 4 Chrome stable majors behind", r.stdout)
        self.assertIn(f"::error::CEF {CEF} is 6 Chrome stable majors behind", r.stdout)

    def test_stale_pins_only_warn_on_a_branch(self):
        r = self.gate("--cef", CEF, url=self.answer("156.0.8078.73"))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual("\n".join(milestone_lines(r.stdout)).count("::warning::"), 2)

    def test_warn_mode_passes_a_stale_tag(self):
        r = self.gate("--tag", "--cef", CEF, url=self.answer("156.0.8078.73"), enforce="warn")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual("\n".join(milestone_lines(r.stdout)).count("::warning::"), 2)

    def test_cef_warn_keeps_a_stale_cef_at_a_warning_while_v8_is_enforced(self):
        # ENGINE_STALENESS_ENFORCE_CEF=warn (DENEXT.md item 218;
        # denext_runtime.yml now enforces CEF too).
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

    def test_current_patch_levels_pass_a_tag(self):
        self.answers()
        r = self.gate("--tag", "--cef", CEF156, "--stable", "156", "--now", "2026-11-03T00:00:00Z")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertNotIn("::", r.stdout)
        self.assertIn("V8 15.6.75.33 trails its branch's patches by at most 14 days", r.stdout)
        self.assertIn("newest patch 15.6.75.35 is 1 day newer", r.stdout)
        self.assertIn(f"CEF {CEF156} is on its branch's newest patch", r.stdout)

    def test_a_patch_lag_past_14_days_fails_a_tag(self):
        # 15.6.75.34 (2026-11-02) has been out more than 14 days by 11-17.
        self.answers()
        r = self.gate("--tag", "--cef", CEF156, "--stable", "156", "--now", "2026-11-17T00:00:00Z")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::V8 15.6.75.33 is more than 14 days behind its branch's patches", r.stdout)
        self.assertIn("has been out 14 days", r.stdout)  # 14 days and 15 hours
        self.assertIn(f"CEF {CEF156} is on its branch's newest patch", r.stdout)

    def test_a_patch_lag_only_warns_on_a_branch(self):
        self.answers()
        r = self.gate("--stable", "156", "--now", "2026-12-01T00:00:00Z")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("::warning::V8 15.6.75.33 is more than 14 days behind", r.stdout)

    def test_a_cef_patch_lag_fails_a_tag_when_cef_is_enforced(self):
        # denext_runtime.yml enforces both engines at a tag.
        self.answers()
        r = self.gate("--tag", "--cef", CEF, "--stable", "151", "--now", "2026-11-03T00:00:00Z", cef="tag")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn(f"::error::CEF {CEF} is more than 14 days behind its branch's patches", r.stdout)
        self.assertIn("newest patch 150.0.23+g4ccc4ca+chromium-150.0.7871.287 is 38 days newer", r.stdout)
        r = self.gate("--tag", "--cef", CEF, "--stable", "151", "--now", "2026-11-03T00:00:00Z", cef="warn")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(f"::warning::CEF {CEF} is more than 14 days behind", r.stdout)

    def test_a_pin_older_than_the_log_is_dated_by_its_tag(self):
        self.answers(log=log_answer(("Version 15.6.75.40", "Mon Nov 30 09:00:00 2026")))
        self.file("tag-15.6.75.33.json", gitiles(json.dumps({"committer": {"time": "Sat Oct 31 13:36:25 2026"}})))
        r = self.gate("--tag", "--stable", "156", "--now", "2026-12-01T00:00:00Z")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("newest patch 15.6.75.40 is 29 days newer", r.stdout)

    def test_v8_version_flag_skips_the_header(self):
        self.answers()
        self.patch_urls["V8_PIN_HEADER_URL"] = CLOSED
        r = self.gate("--tag", "--stable", "156", "--v8-version", "15.6.75.35", "--now", "2027-01-01T00:00:00Z")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("V8 15.6.75.35 is on its branch's newest patch", r.stdout)

    def test_an_unreadable_v8_version_flag_fails(self):
        r = self.gate("--stable", "156", "--v8-version", "15.6")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::--v8-version is '15.6'", r.stdout)

    def test_unknown_patch_levels_warn_and_pass_a_tag(self):
        pathlib.Path(self.lock).write_text(GIT_LOCK)
        r = self.gate("--tag", "--cef", CEF156, "--stable", "156")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("::warning::could not learn the patch level of V8 15.6 (v8 crate 156.0.0+denext.3)", r.stdout)
        self.assertIn(f"::warning::could not learn the patch level of CEF {CEF156}", r.stdout)

    def test_a_cef_pin_missing_from_the_index_is_unknown(self):
        self.answers()
        r = self.gate("--tag", "--cef", "156.0.99+gabc+chromium-156.0.1.2", "--stable", "156", "--now", "2026-11-03T00:00:00Z")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("is not in CEF's build index", r.stdout)

    def test_offline_stable_still_checks_the_patch_level(self):
        self.answers()
        r = self.gate("--tag", "--now", "2026-12-01T00:00:00Z", url=CLOSED)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::warning::could not learn the Chrome stable milestone", r.stdout)
        self.assertIn("::error::V8 15.6.75.33 is more than 14 days behind", r.stdout)

    def test_a_lock_without_v8_fails(self):
        pathlib.Path(self.lock).write_text('[[package]]\nname = "serde"\nversion = "1.0.0"\n')
        r = self.gate("--stable", "152")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("::error::no readable v8 package version", r.stdout)


if __name__ == "__main__":
    unittest.main()
