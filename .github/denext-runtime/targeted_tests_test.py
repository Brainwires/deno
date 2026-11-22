#!/usr/bin/env python3
"""Tests for targeted_tests.py, the targeted-test mode of denext_tests.yml
(`only` / `os`): the selector syntax, the per-leg plan the test and libs jobs
run, and that its lists agree with the workflows.

Usage: python3 -I .github/denext-runtime/targeted_tests_test.py
"""

import importlib.util
import io
import json
import os
import re
import unittest
from contextlib import redirect_stdout

HERE = os.path.dirname(os.path.abspath(__file__))
WORKFLOWS = os.path.join(HERE, "..", "workflows")

spec = importlib.util.spec_from_file_location(
    "targeted_tests", os.path.join(HERE, "targeted_tests.py")
)
tt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tt)


def read(name):
    with open(os.path.join(WORKFLOWS, name), encoding="utf-8") as f:
        return f.read()


def run_main(cmd, **env):
    out = io.StringIO()
    with redirect_stdout(out):
        code = tt.main(["targeted_tests.py", cmd], env)
    return code, out.getvalue()


class ParseOnly(unittest.TestCase):
    def test_the_documented_examples(self):
        items = tt.parse_only(
            "specs:kernel_sandbox::policy::config unit:pty_test\n"
            "unit_node:dgram_test libs:deno_permissions:kernel_broker::net\n"
            "integration:run::foo node_compat:parallel/test-dgram-bind.js"
        )
        self.assertEqual(
            items,
            [
                ("specs", None, "kernel_sandbox::policy::config"),
                ("unit", None, "pty_test"),
                ("unit_node", None, "dgram_test"),
                ("libs", "deno_permissions", "kernel_broker::net"),
                ("integration", None, "run::foo"),
                ("node_compat", None, "parallel/test-dgram-bind.js"),
            ],
        )

    def test_a_suite_without_a_filter_runs_whole(self):
        self.assertEqual(tt.parse_only("specs"), [("specs", None, "")])
        self.assertEqual(
            tt.parse_only("libs:deno_core"), [("libs", "deno_core", "")]
        )

    def test_duplicates_are_dropped(self):
        self.assertEqual(
            tt.parse_only("unit:a  unit:a\nunit:b"),
            [("unit", None, "a"), ("unit", None, "b")],
        )

    def test_invalid_items_are_refused(self):
        for text, why in (
            ("", "names no test"),
            ("   \n", "names no test"),
            ("lint", "unknown suite"),
            ("spec:foo", "unknown suite"),
            ("unit:", "empty filter"),
            ("libs", "names its crate"),
            ("libs:", "names its crate"),
            ("libs:not_a_crate:x", "is not a crate"),
            ("libs:../x", "is not a crate"),
            ("specs:-x", "the filter may hold"),
            ("specs:$(id)", "the filter may hold"),
            ("specs:a;b", "the filter may hold"),
            ("specs:a`b`", "the filter may hold"),
            ("specs:a\"b", "the filter may hold"),
            ("specs:*", "the filter may hold"),
        ):
            with self.assertRaises(tt.Invalid, msg=text) as cm:
                tt.parse_only(text)
            self.assertIn(why, str(cm.exception), text)

    def test_too_many_test_items(self):
        text = " ".join(f"unit:t{n}" for n in range(tt.MAX_TEST_ITEMS + 1))
        with self.assertRaises(tt.Invalid):
            tt.parse_only(text)
        # Lib items are steps of one job: not counted.
        text = " ".join(f"unit:t{n}" for n in range(tt.MAX_TEST_ITEMS))
        tt.parse_only(text + " libs:deno_fs:a libs:deno_fs:b")


class ParseOs(unittest.TestCase):
    def test_names_map_to_legs_in_order(self):
        self.assertEqual(
            tt.parse_os("windows-arm,linux,macos-x64"),
            ["ubuntu-22.04", "macos-15-intel", "windows-11-arm"],
        )
        self.assertEqual(
            tt.parse_os(",".join(tt.LEGS)), list(tt.LEGS.values())
        )

    def test_unknown_or_spaced_names_are_refused(self):
        for text in ("", "linux, windows", "ubuntu-22.04", "mac", "linux,"):
            with self.assertRaises(tt.Invalid, msg=text):
                tt.parse_os(text)

    def test_the_legs_are_the_release_gates(self):
        spec = importlib.util.spec_from_file_location(
            "tag_gate", os.path.join(HERE, "tag_gate.py")
        )
        gate = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(gate)
        self.assertEqual(sorted(tt.LEGS.values()), sorted(gate.REQUIRED_LEGS))

    def test_each_leg_matches_its_os_name(self):
        # denext_tests.yml: each leg's `if:` matches its name in `,<os>,`,
        # and the leg's `name:` is the image LEGS maps it to.
        text = read("denext_tests.yml")
        for name, leg in tt.LEGS.items():
            m = re.search(
                rf"\n    name: {re.escape(leg)}\n    needs: guard\n    if: (.*)\n",
                text,
            )
            self.assertIsNotNone(m, leg)
            self.assertIn(
                f"contains(format(',{{0}},', inputs.os), ',{name},')",
                m.group(1),
                leg,
            )
            for other in tt.LEGS:
                if other != name:
                    self.assertNotIn(f"',{other},'", m.group(1), leg)


class Plan(unittest.TestCase):
    def test_matrix_libs_and_keys(self):
        matrix, libs, keys = tt.plan(
            tt.parse_only(
                "specs:kernel_sandbox::policy libs:deno_permissions:net"
                " unit libs:deno_core node_compat:test-dgram"
            )
        )
        self.assertEqual(
            matrix,
            [
                {
                    "suite": "specs",
                    "package": "specs_tests",
                    "shard": "",
                    "shard_total": "",
                    "shard_label": " [kernel_sandbox::policy]",
                    "pick": "specs_t0",
                    "filter": "kernel_sandbox::policy",
                    "target": "-t0",
                },
                {
                    "suite": "unit",
                    "package": "unit_tests",
                    "shard": "",
                    "shard_total": "",
                    "shard_label": " (whole suite)",
                    "pick": "unit_t1",
                    "filter": "",
                    "target": "-t1",
                },
                {
                    "suite": "node_compat",
                    "package": "node_compat_tests",
                    "shard": "",
                    "shard_total": "",
                    "shard_label": " [test-dgram]",
                    "pick": "node_compat_t2",
                    "filter": "test-dgram",
                    "target": "-t2",
                },
            ],
        )
        self.assertEqual(libs, ["deno_permissions:net", "deno_core"])
        self.assertEqual(keys, ["libs", "specs_t0", "unit_t1", "node_compat_t2"])

    def test_pick_keys_keep_the_pool_choice_patterns(self):
        # test-runner sends `specs_*`, `node_compat_*` and `libs` to the fast
        # Windows host; a targeted key must still match those patterns.
        _, _, keys = tt.plan(tt.parse_only("specs:a node_compat:b libs:deno_fs"))
        self.assertTrue(keys[1].startswith("specs_"))
        self.assertTrue(keys[2].startswith("node_compat_"))
        self.assertEqual(keys[0], "libs")
        text = read("denext_tests_os.yml")
        self.assertIn('[[ "$k" == specs_* ]] || [[ "$k" == node_compat_* ]] || [ "$k" = libs ]', text)

    def test_lib_commands(self):
        self.assertEqual(
            tt.lib_command("deno_permissions:kernel_broker::net"),
            "cargo test --locked --lib -p deno_permissions -p deno_v8 --"
            " kernel_broker::net".split(),
        )
        self.assertEqual(
            tt.lib_command("deno_core"),
            "cargo test --locked --lib -p deno_core".split(),
        )
        self.assertEqual(
            tt.lib_command("deno_v8:x"),
            "cargo test --locked --lib -p deno_v8 -- x".split(),
        )

    def test_plan_output(self):
        code, out = run_main("plan", ONLY="unit:pty_test libs:deno_fs")
        self.assertEqual(code, 0, out)
        lines = dict(line.split("=", 1) for line in out.splitlines())
        self.assertEqual(lines["tests"], "true")
        self.assertEqual(lines["libs"], "deno_fs")
        self.assertEqual(lines["keys"], "libs unit_t0")
        self.assertEqual(json.loads(lines["matrix"])[0]["filter"], "pty_test")
        code, out = run_main("plan", ONLY="libs:deno_fs")
        lines = dict(line.split("=", 1) for line in out.splitlines())
        self.assertEqual(lines["tests"], "false")
        self.assertEqual(lines["matrix"], "[]")

    def test_check_and_errors(self):
        code, out = run_main("check", ONLY="specs:foo", OS="linux,windows")
        self.assertEqual(code, 0, out)
        self.assertIn("ubuntu-22.04, windows-2022", out)
        self.assertIn("cargo test -p specs_tests --test specs -- foo", out)
        code, out = run_main("check", ONLY="specs:foo", OS="linux, windows")
        self.assertEqual(code, 1)
        self.assertIn("::error::targeted run:", out)
        code, out = run_main("plan", ONLY="bogus")
        self.assertEqual(code, 1)
        self.assertTrue(out.startswith("::error::"), out)


class Workflows(unittest.TestCase):
    def test_lib_crates_match_the_test_libs_step(self):
        text = read("denext_tests_os.yml")
        m = re.search(
            r"- name: Test libs\n        id: libs\n.*\n        run: cargo test --locked --lib (.*)\n",
            text,
        )
        self.assertIsNotNone(m)
        crates = re.findall(r"-p (\S+)", m.group(1))
        self.assertEqual(crates, tt.LIB_CRATES)

    def test_core_crates_match_the_core_libs_step(self):
        text = read("denext_tests_os.yml")
        m = re.search(r"- name: Test core libs\n(?:.*\n){2}\s+(cargo test .*)\n", text)
        self.assertIsNotNone(m)
        self.assertEqual(
            tuple(re.findall(r"-p (\S+)", m.group(1))), tt.CORE_LIB_CRATES
        )
        # The targeted step runs the core crates (and deno_v8) without
        # adding deno_v8, as lib_command does.
        self.assertIn(
            "deno_core | serde_v8 | deno_ops | deno_v8) ;;", text
        )

    def test_suites_match_the_matrix_packages(self):
        text = read("denext_tests_os.yml")
        pairs = set(
            re.findall(r'\{"suite":"(\w+)","package":"(\w+)"', text)
        )
        self.assertEqual(pairs, set(tt.SUITES.items()))


if __name__ == "__main__":
    unittest.main()
