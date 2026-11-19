# Tools

Documentation for various tooling in support of Deno development.

## format.js

This script will format the code (currently using dprint, rustfmt). It is a
prerequisite to run this before code check in.

To run formatting:

```sh
deno run --allow-read --allow-write --allow-run ./tools/format.js
```

## lint.js

This script will lint the code base (currently using deno lint, clippy). It is a
prerequisite to run this before code check in.

To run linting:

```sh
deno run --allow-read --allow-write --allow-run ./tools/lint.js
```

Tip: You can also use cargo to run the current or pending build of the deno
executable

```sh
cargo run -- run --allow-read --allow-write --allow-run ./tools/<script>
```

## wgpu_sync.js

`wgpu_sync.js` streamlines updating `deno_webgpu` from
[gfx-rs/wgpu](https://github.com/gfx-rs/wgpu/).

It essentially vendors the `deno_webgpu` tree with a few minor patches applied
on top, somewhat similar to `git subtree`.

1. Update `COMMIT` or `V_WGPU` in `./tools/wgpu_sync.js`
2. Run `./tools/wgpu_sync.js`
3. Double check changes, possibly patch
4. Commit & send a PR with the updates

## copyright_checker.js

`copyright_checker.js` is used to check copyright headers in the codebase.

To run the _copyright checker_:

```sh
deno run --allow-read --allow-run  ./tools/copyright_checker.js
```

Then it will check all code files in the repository and report any files that
are not properly licensed.

## denext/flake_hunt.ts

The flake hunt (denext): runs the fork's test suites the way the `denext tests`
gate runs them, many times over, on our own machines, before a runtime tag. The
release gate (`.github/denext-runtime/tag_gate.py`) wants a `denext tests` run
green on its first attempt for the tagged commit, so a test that fails one run
in ten costs a ~3 h gate restart there; here it costs a line in a report. Fix
every flake it finds before tagging.

```sh
# on the Mac mini, from the fork's checkout: run on biscuits, wait, and write
# the release marker here when clean
deno run -A tools/denext/flake_hunt.ts --host biscuits --mode fast --suite libs,unit
deno run -A tools/denext/flake_hunt.ts --host biscuits --mode full
# re-attach to a run after the local side stopped
deno run -A tools/denext/flake_hunt.ts --host biscuits --attach <run-id>
# on the host itself (Linux: containers; macOS: nice -n 19, one suite at a time)
deno run -A tools/denext/flake_hunt.ts --commit <sha> --plan
```

What it does:

- **Builds once per commit** in `~/flake-hunt/c/<sha12>` (one cached commit at a
  time), as the gate's build and test jobs build: upstream's `Build debug`
  command, `cargo build -p test_ffi`, then `cargo test --no-run` with the exact
  arguments of the `Test libs` and core lib steps (read from
  `.github/workflows/denext_tests_os.yml`, so they stay in step) and of each
  harness suite (`unit`, `unit_node`, `specs`, `integration`, `node_compat`). On
  Linux the build runs in the CI runner image under `~/.fork-cargo.lock`.
- **Runs the built test binaries** N times (`--reps`, default 10) with the
  gate's environment (`CI=true`, so the harness retries and writes its JSON
  report, `CARGO_PROFILE_DEV_DEBUG=0`, `RUST_BACKTRACE=full`, ...), each binary
  from its package directory with the `CARGO_*` variables `cargo test` sets, and
  the sharded suites per shard (`CI_SHARD_INDEX` / `CI_SHARD_TOTAL`). Lib test
  binaries run one after another per repetition, as `cargo test --lib` runs
  them, but on past a failure.
- **Isolates the repetitions**: on Linux every run is its own container with the
  Linux pool's job profile (`denext-internal` `ops/runners/linux/run-job.sh`:
  the runner image, uid 1001, the job seccomp and AppArmor profiles, the
  `denext-ci` network with IPv6 link-local at `172.30.0.200` and up, the hashy
  override, 3500 MB of memory, `FILE_TEST_RUNNER_PARALLELISM=2`, the whole-host
  CPU quota) at `--cpu-shares 256`, half a runner job's, and starts from a fresh
  hard-link copy of the checkout, so parallel runs never share a port, a
  directory or a pid namespace; each ends with the gate's "no git changes"
  check. `--workers` runs (default 2) go at once, the suites interleaved, so the
  tests run under load. While our runner jobs are busy on the host the hunt
  keeps to fewer workers, and it waits while memory is under 4 GB or the disk
  under 35 GB.
- **Reports** every test that failed at least once: in how many repetitions, how
  (`failed`; `retried`: the harness's CI retry passed it, which a gate run
  survives but which is still a flake; `crashed`: a test binary died without
  naming a test; `timeout`; `git-dirty`), and the first failure's output
  (`report.md` and `report.json` in the run directory, every log under `logs/`).
  Exit 0 when clean, 1 on any flake, 2 on an infrastructure error.

`--mode fast` (the default) runs what the train's commits touch since the
nearest `denext-runtime-v*` tag below the candidate (`--base` overrides): a
changed `tests/unit/*_test.ts` runs that file, a `tests/specs/` change its spec
directory, `tests/integration/<x>_tests.rs` that module, `tests/unit_node/` and
`ext/node/` JS the unit_node suite, `tests/node_compat/` the node_compat suite,
an extension's JS its unit test files, and a Rust source file its module's lib
tests (the crate from `cargo metadata`, the module path from the file's place in
the crate). It always adds the known-flaky list (`KNOWN_FLAKY` in the script:
tests that flaked in a gate before) at `--flaky-reps` (default 30). `--suite`
adds whole suites; `--mode full` runs every suite in full.

**The release marker.** A clean run that is full, or fast with at least
`--suite libs,unit`, writes `~/.denext-runners/flake-hunt/<full sha>.ok` on the
machine that started it (with `--host biscuits`: the Mac mini), a JSON record of
the mode, suites, repetitions, hosts and times. A run removes that commit's old
marker when it starts and writes nothing when it finds a flake. On the mini a
hook refuses to dispatch the `denext tests` workflow unless the marker of the
exact commit the ref points at exists (`denext-internal`
`ops/release/gate-guard-hook.py`).
