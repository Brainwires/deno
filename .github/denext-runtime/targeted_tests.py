#!/usr/bin/env python3
"""The targeted-test mode of denext_tests.yml: run only the named tests, on
the named OS legs, to check one fix in minutes instead of the full matrix.

A dispatch with `only` set runs, on each leg `os` names, the leg's debug build
and then only the selected suites, each filtered; every other job (lint,
cargo audit, the other suites and shards, the webrtc mDNS test) is skipped.
The release gate (tag_gate.py `is_targeted`) and the verified-SHA guard leave
such a run out: it never vouches for a release and a red one blocks nothing.

`only`: items separated by spaces or newlines, each `<suite>[:<filter>]`:

  specs:kernel_sandbox::policy::config   cargo test -p specs_tests --test specs -- <filter>
  unit:pty_test                          cargo test -p unit_tests --test unit -- <filter>
  unit_node:dgram_test                   cargo test -p unit_node_tests --test unit_node -- <filter>
  integration:<filter>                   cargo test -p integration_tests --test integration -- <filter>
  node_compat:<file>                     cargo test -p node_compat_tests --test node_compat -- <file>
  libs:<crate>[:<filter>]                cargo test --locked --lib -p <crate> -p deno_v8 [-- <filter>]

The test suites' harnesses (file_test_runner) take one filter, the first
argument after `--`, and run every test whose name contains it; an item
without a filter runs that suite whole, in one job (no shards). node_compat
with a filter runs any matching test of Node's suite, also one config.jsonc
does not enable. Each test item is its own job; the lib items run one after
another in the leg's one `test libs` job. A lib crate must be one the full
run tests (LIB_CRATES, the `Test libs` step of denext_tests_os.yml, or
CORE_LIB_CRATES, its `Test core libs` step); deno_v8 goes in with the first
list because on its own Cargo does not unify its `v8` feature in. A filter
is libtest's substring filter. Filters are limited to FILTER's characters
(no spaces, no shell syntax, no leading `-`).

`os`: comma-separated legs, no spaces: linux (ubuntu-22.04), linux-arm
(ubuntu-22.04-arm), macos (macos-15), macos-x64 (macos-15-intel), windows
(windows-2022), windows-arm (windows-11-arm). The workflow's `if:`s match
these names in `,<os>,`; `check` refuses anything else.

Usage: targeted_tests.py check     (env ONLY, OS) validate both, print a summary
       targeted_tests.py plan      (env ONLY) print the GITHUB_OUTPUT lines:
         matrix=<JSON include list for the leg's test job>
         tests=true|false
         libs=<space-separated crate[:filter] items, empty for none>
         keys=<space-separated pick-runners keys of those jobs>
Exits 1 with an `::error::` line on any invalid item.
"""

import json
import os
import re
import sys

# The test suites: suite -> cargo package (denext_tests_os.yml's matrix).
SUITES = {
    "unit": "unit_tests",
    "unit_node": "unit_node_tests",
    "specs": "specs_tests",
    "integration": "integration_tests",
    "node_compat": "node_compat_tests",
}
# denext_tests_os.yml's `Test libs` crates (targeted_tests_test.py checks the
# two agree) and its `Test core libs` ones.
LIB_CRATES = (
    "deno denort node_shim bsdiff_helper deno_lib denort_desktop deno_snapshots"
    " deno_bundle_runtime deno_cache deno_canvas deno_cron deno_crypto"
    " deno_fetch deno_ffi deno_fs deno_http deno_image deno_io deno_kv"
    " deno_napi napi_sym deno_net deno_node deno_node_crypto deno_node_sqlite"
    " deno_sql deno_pty denort_helper deno_signals deno_telemetry deno_url"
    " deno_web deno_webgpu deno_webidl deno_webrtc deno_websocket"
    " deno_webstorage deno_cache_dir deno_cli_parser deno_config"
    " deno_crypto_provider deno_v8 deno_dotenv eszip deno_http_h1"
    " deno_inspector_server deno_lockfile deno_maybe_sync napi_sys"
    " node_resolver deno_npm deno_npm_cache deno_npm_installer deno_npmrc"
    " deno_package_json deno_print deno_resolver deno_sandbox deno_runtime"
    " deno_features deno_permissions deno_subprocess_windows deno_os"
    " deno_process deno_html_rewriter deno_tls test_util"
).split()
CORE_LIB_CRATES = ("deno_core", "serde_v8", "deno_ops")
# The `os` names -> the leg (its runner image name, the caller job's name).
LEGS = {
    "linux": "ubuntu-22.04",
    "linux-arm": "ubuntu-22.04-arm",
    "macos": "macos-15",
    "macos-x64": "macos-15-intel",
    "windows": "windows-2022",
    "windows-arm": "windows-11-arm",
}
FILTER = re.compile(r"[A-Za-z0-9_.:/@+=][A-Za-z0-9_.:/@+=-]*")
CRATE = re.compile(r"[A-Za-z0-9_-]+")
# At most this many test jobs per leg (each builds nothing, but takes a
# runner).
MAX_TEST_ITEMS = 20


class Invalid(Exception):
    pass


def parse_only(text):
    """[(suite, crate or None, filter)] from `only`, duplicates dropped, in
    order. Raises Invalid."""
    items = []
    for raw in (text or "").split():
        suite, sep, rest = raw.partition(":")
        crate = None
        if suite == "libs":
            crate, _, rest = rest.partition(":")
            if not sep or not crate:
                raise Invalid(
                    f"'{raw}': a libs item names its crate (libs:<crate>[:<filter>])"
                )
            if not CRATE.fullmatch(crate) or (
                crate not in LIB_CRATES and crate not in CORE_LIB_CRATES
            ):
                raise Invalid(
                    f"'{raw}': '{crate}' is not a crate the lib tests run"
                    f" ({' '.join(LIB_CRATES + list(CORE_LIB_CRATES))})"
                )
        elif suite not in SUITES:
            raise Invalid(
                f"'{raw}': unknown suite '{suite}' (one of"
                f" {', '.join(list(SUITES) + ['libs'])})"
            )
        elif sep and not rest:
            raise Invalid(f"'{raw}': an empty filter (leave out the ':')")
        if rest and not FILTER.fullmatch(rest):
            raise Invalid(
                f"'{raw}': the filter may hold only letters, digits and"
                " _ . : / @ + = - (not first)"
            )
        item = (suite, crate, rest)
        if item not in items:
            items.append(item)
    if not items:
        raise Invalid("`only` names no test")
    tests = [i for i in items if i[0] != "libs"]
    if len(tests) > MAX_TEST_ITEMS:
        raise Invalid(
            f"{len(tests)} test items: at most {MAX_TEST_ITEMS} (each is a job)"
        )
    return items


def parse_os(text):
    """The legs `os` names, in LEGS order. Raises Invalid."""
    names = (text or "").split(",")
    unknown = [n for n in names if n not in LEGS]
    if unknown:
        raise Invalid(
            f"`os` '{text}': unknown leg(s) {', '.join(map(repr, unknown))}"
            f" (comma-separated, no spaces: {','.join(LEGS)})"
        )
    return [LEGS[n] for n in LEGS if n in names]


def plan(items):
    """(matrix include list, libs items, pick keys) for one leg."""
    matrix, libs, keys = [], [], []
    for suite, crate, flt in items:
        if suite == "libs":
            libs.append(f"{crate}:{flt}" if flt else crate)
            continue
        n = len(matrix)
        key = f"{suite}_t{n}"
        matrix.append(
            {
                "suite": suite,
                "package": SUITES[suite],
                "shard": "",
                "shard_total": "",
                "shard_label": f" [{flt}]" if flt else " (whole suite)",
                "pick": key,
                "filter": flt,
                "target": f"-t{n}",
            }
        )
        keys.append(key)
    if libs:
        keys.insert(0, "libs")
    return matrix, libs, keys


def lib_command(item):
    """The cargo command (argv) for one `libs` item, `crate[:filter]`: what
    the `Test libs (targeted)` step of denext_tests_os.yml runs in bash (it
    runs on the leg's own runner, which may have no Python;
    targeted_tests_test.py checks the step's core crate list)."""
    crate, _, flt = item.partition(":")
    argv = ["cargo", "test", "--locked", "--lib", "-p", crate]
    if crate in LIB_CRATES and crate != "deno_v8":
        argv += ["-p", "deno_v8"]
    if flt:
        argv += ["--", flt]
    return argv


def main(argv, env):
    if len(argv) != 2 or argv[1] not in ("check", "plan"):
        print(__doc__, file=sys.stderr)
        return 2
    try:
        items = parse_only(env.get("ONLY", ""))
        if argv[1] == "check":
            legs = parse_os(env.get("OS", ""))
            matrix, libs, _ = plan(items)
            print(f"Targeted run on {', '.join(legs)}:")
            for entry in matrix:
                print(
                    f"- test {entry['suite']}{entry['shard_label']}: cargo test"
                    f" -p {entry['package']} --test {entry['suite']}"
                    + (f" -- {entry['filter']}" if entry["filter"] else "")
                )
            for item in libs:
                print(f"- test libs: {' '.join(lib_command(item))}")
            return 0
        matrix, libs, keys = plan(items)
        print("matrix=" + json.dumps(matrix, separators=(",", ":")))
        print(f"tests={'true' if matrix else 'false'}")
        print("libs=" + " ".join(libs))
        print("keys=" + " ".join(keys))
        return 0
    except Invalid as e:
        print(f"::error::targeted run: {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv, os.environ))
