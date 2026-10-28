#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests for assert_guard.py, the lint check that every assertion in the
release and test scripts names what it checked: bare `test` / `[` / `[[` /
`grep -q` statements in shell files and workflow `run:` blocks are refused;
named ones (`need`, `|| { echo ...; exit 1; }`) and conditions pass.

Usage: python3 -I .github/denext-runtime/assert_guard_test.py
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
GUARD = os.path.join(HERE, "assert_guard.py")

spec = importlib.util.spec_from_file_location("assert_guard", GUARD)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def found(name, text):
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, name)
        with open(path, "w", encoding="utf-8") as f:
            f.write(text)
        return [line.split(":", 2)[1:] for line in guard.check_file(path)]


class Shell(unittest.TestCase):
    def test_bare_assertions_are_refused(self):
        script = "\n".join(
            [
                "set -euo pipefail",
                'test -f "$out"',
                '[ "$a" = "$b" ]',
                '[[ -n "$x" ]]',
                'grep -q "needle" file',
                'grep -Eq "a|b" file',
                "! test -e stale",
                'test -f a && test -f b',
                'test -n "$v" || exit 1',
                'test -n "$v" || { exit 1; }',
            ]
        )
        lines = [int(n) for n, _ in found("x.sh", script)]
        self.assertEqual(lines, [2, 3, 4, 5, 6, 7, 8, 9, 10])

    def test_named_checks_and_conditions_pass(self):
        script = "\n".join(
            [
                'need "the archive" test -f "$out"',
                'test -f "$out" || { echo "::error::no $out" >&2; exit 1; }',
                'grep -q x f ||',
                '  { echo "::error::no x in f" >&2; exit 1; }',
                'test -f "$a" || {',
                '  echo "::error::no $a" >&2',
                "  exit 1",
                "}",
                'if [ -f "$a" ]; then echo y; fi',
                'while [ "$n" -lt 3 ]; do n=$((n+1)); done',
                '[ -f "$a" ] && break',
                '[ -z "$e" ] || args+=(--entitlements "$e")',
                '[ -d "$d" ] || continue',
                "# test -f commented",
                "testing=1",
                "[[:space:]]",
            ]
        )
        self.assertEqual(found("x.sh", script), [])

    def test_heredoc_bodies_are_not_statements(self):
        script = "\n".join(
            [
                "cat >notes <<'EOF'",
                "test -f inside-a-heredoc",
                "EOF",
                'x=$(tr a b <<<"$y")',
                "test -f after",
            ]
        )
        self.assertEqual([int(n) for n, _ in found("x.sh", script)], [5])

    def test_a_check_after_a_continuation_is_not_a_statement(self):
        script = "cmd --flag \\\n  test -f not-a-statement\n"
        self.assertEqual(found("x.sh", script), [])


class Workflow(unittest.TestCase):
    def test_run_blocks_are_checked_with_their_line_numbers(self):
        workflow = "\n".join(
            [
                "jobs:",
                "  a:",
                "    steps:",
                "      - name: literal",
                "        run: |",
                "          echo start",
                '          test -n "$v"',
                "      - name: one line",
                '        run: test -f "$x"',
                "      - name: named",
                "        run: |",
                '          test -n "$v" || { echo "::error::v is empty"; exit 1; }',
                "      - name: folded",
                "        run: >-",
                "          cargo test --locked",
                "          test-name",
                "        env:",
                "          test: 1",
            ]
        )
        self.assertEqual([int(n) for n, _ in found("w.yml", workflow)], [7, 9])


class Repository(unittest.TestCase):
    def test_the_repository_has_no_bare_assertion(self):
        result = subprocess.run(
            [sys.executable, "-I", GUARD], capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_the_cli_fails_on_a_bare_assertion(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "bad.sh")
            with open(path, "w", encoding="utf-8") as f:
                f.write('set -e\ntest -f "$out"\n')
            result = subprocess.run(
                [sys.executable, "-I", GUARD, path], capture_output=True, text=True
            )
        self.assertEqual(result.returncode, 1)
        self.assertIn('bad.sh:2: test -f "$out"', result.stdout)
        self.assertIn("::error::1 bare assertion", result.stderr)


if __name__ == "__main__":
    unittest.main()
