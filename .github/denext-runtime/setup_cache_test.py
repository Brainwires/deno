#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests for the Rust cache step of .github/denext-tests/setup/action.yml: it
saves only from a GitHub-hosted image (a self-hosted pool's upload ran for an
hour in a job's post step, which no timeout-minutes bounds) and bounds a
restore's download.

Usage: python3 -I .github/denext-runtime/setup_cache_test.py
"""

import os
import re
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ACTION = os.path.join(HERE, "..", "denext-tests", "setup", "action.yml")
USES = "- uses: Swatinem/rust-cache@"


def cache_steps(text):
    """Each rust-cache step's lines: from its `- uses:` to the next step."""
    lines = text.splitlines()
    steps = []
    for i, line in enumerate(lines):
        if line.strip().startswith(USES):
            indent = len(line) - len(line.lstrip())
            body = []
            for nxt in lines[i + 1:]:
                stripped = nxt.strip()
                if stripped and (len(nxt) - len(nxt.lstrip())) <= indent:
                    break
                body.append(stripped)
            steps.append(body)
    return steps


def check(text):
    """The reasons the action's cache steps are unsafe on a pool ([] = safe)."""
    steps = cache_steps(text)
    if not steps:
        return ["no Swatinem/rust-cache step found"]
    errors = []
    for n, body in enumerate(steps, 1):
        if "save-if: ${{ runner.environment == 'github-hosted' }}" not in body:
            errors.append(f"rust-cache step {n} saves from self-hosted runners")
        pattern = r"SEGMENT_DOWNLOAD_TIMEOUT_MINS: '?(\d+)'?$"
        minutes = [m.group(1) for m in (re.match(pattern, x) for x in body) if m]
        if not minutes or not 1 <= int(minutes[0]) <= 10:
            errors.append(
                f"rust-cache step {n} has no SEGMENT_DOWNLOAD_TIMEOUT_MINS of 1-10")
    return errors


class SetupCacheTest(unittest.TestCase):

    def test_action(self):
        with open(ACTION, encoding="utf-8") as f:
            self.assertEqual(check(f.read()), [])

    def test_unbounded_save_is_refused(self):
        text = """runs:
  steps:
    - uses: Swatinem/rust-cache@abc # v2
      env:
        SEGMENT_DOWNLOAD_TIMEOUT_MINS: '5'
      with:
        key: x
    - name: next
"""
        self.assertEqual(
            check(text), ["rust-cache step 1 saves from self-hosted runners"]
        )

    def test_unbounded_restore_is_refused(self):
        text = """    - uses: Swatinem/rust-cache@abc # v2
      with:
        save-if: ${{ runner.environment == 'github-hosted' }}
    - name: next
      env:
        SEGMENT_DOWNLOAD_TIMEOUT_MINS: '5'
"""
        self.assertEqual(
            check(text),
            ["rust-cache step 1 has no SEGMENT_DOWNLOAD_TIMEOUT_MINS of 1-10"],
        )

    def test_no_cache_step_is_refused(self):
        self.assertEqual(
            check("runs:\n  steps: []\n"), ["no Swatinem/rust-cache step found"]
        )


if __name__ == "__main__":
    unittest.main()
