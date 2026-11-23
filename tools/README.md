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
# on the Mac mini, from the fork's checkout: biscuits takes 4 shares of the
# repetitions, the mini's Docker Desktop 1; wait, then write the release marker
deno run -A tools/denext/flake_hunt.ts --host biscuits:4,local:1 --mode fast --suite libs,unit
deno run -A tools/denext/flake_hunt.ts --host biscuits:4,local:1 --mode full
# re-attach after the waiting side stopped (the hosts keep running)
deno run -A tools/denext/flake_hunt.ts --host biscuits:4,local:1 --attach <run-id>
# what would run (builds first)
deno run -A tools/denext/flake_hunt.ts --host biscuits --plan
```

What it does:

- **Builds once per commit** in `~/flake-hunt/c/<sha12>` (one cached commit at a
  time) in the CI runner image, as the gate's build and test jobs build:
  upstream's `Build debug` command, `cargo build -p test_ffi`, then
  `cargo test --no-run` with the exact arguments of the `Test libs` and core lib
  steps (read from `.github/workflows/denext_tests_os.yml`, so they stay in
  step) and of each harness suite (`unit`, `unit_node`, `specs`, `integration`,
  `node_compat`). The prebuilt V8 is verified once and reused, so a second run
  of the same commit builds nothing. On biscuits the build takes
  `~/.fork-cargo.lock`; on the mini the one heavy-build lock every agent there
  shares, `/tmp/claude-501/mini-heavy-build.lock` (held with `lockf`), at
  `-j 4`, under `nice`.
- **Runs the built test binaries** with the gate's environment (`CI=true`, so
  the harness retries and writes its JSON report, `CARGO_PROFILE_DEV_DEBUG=0`,
  `RUST_BACKTRACE=full`, ...), each binary from its package directory with the
  `CARGO_*` variables and library path `cargo test` sets. The unit of repetition
  is small, so the repetitions spread over every worker: each lib test binary
  (with its step's arguments), each test file of `unit` and `unit_node` (the
  harness's own filter, the file list from its `--list`), and each shard of
  `specs`, `integration` and `node_compat` (`--shards`, default 4;
  `CI_SHARD_INDEX` / `CI_SHARD_TOTAL`). Each unit runs `--reps` times
  (default 10) in runs of `--chunk` repetitions (default 5), which the workers
  take in parallel.
- **Isolates the runs**: every run is its own container with our pool's job
  profile and starts from a fresh hard-link copy of the checkout, so parallel
  runs never share a port, a directory or a pid namespace; each repetition ends
  with the gate's "no git changes" check. On biscuits that is the Linux x64
  pool's profile (`denext-internal` `ops/runners/linux/run-job.sh`: the
  `denext-ci-linux-2204` image, uid 1001, the job seccomp and AppArmor profiles,
  the `denext-ci` network with IPv6 link-local at `172.30.0.200` and up, the
  hashy override, 3500 MB, `FILE_TEST_RUNNER_PARALLELISM=2`, the whole-host CPU
  quota) at `--cpu-shares 256`, half a runner job's, with the CPUs minus 2 as
  workers. On the mini it is Docker Desktop with the linux-arm64 pool's image
  (`denext-ci-linux-arm64`, the x64 job seccomp profile so AF_VSOCK works as in
  the gate's test jobs, a 4-CPU quota), one worker, the trees in Docker volumes:
  no test binary runs on macOS itself, so nothing can raise a firewall or
  privacy dialog. A run waits while memory is under 4 GB or the disk under 35
  GB.
- **Schedules by CPU cost**, so the gate's container sees a gate runner's
  contention rather than an over-subscribed host's: a run costs the CPUs its
  tests keep busy at once (a harness run its `FILE_TEST_RUNNER_PARALLELISM` test
  files, each a process wanting a CPU of its own, as `node_compat`, `unit`,
  `unit_node`, `specs` and `integration` run them; a lib test binary its
  `--test-threads`, else the gate container's 3), capped by its container's
  `--cpus`. The runs in flight cost at most the host's CPUs (the Docker VM's on
  the mini) minus what each busy runner job reserves (its `--cpus`: 3 on
  biscuits, 4 on the mini), with at least one run going; `--workers` stays the
  cap on containers. On an 8-CPU biscuits with no runner busy that is four
  `normal` runs of two tests each, not the six (twelve tests) it was.
- **Constrains the CPU** (`--profiles`, default `normal,cpu1,cpu0.5`): besides
  the gate's own container (`normal`, `--reps` times), every unit also runs
  `--constrained-reps` times (default 3) in the same container capped at
  `--cpus=1` and at `--cpus=0.5`, with the harness at the gate's own
  parallelism, so timing races that an idle many-core machine hides surface
  reliably. A `+hog` suffix (`cpu1+hog`), or `--hog` for every constrained
  profile, adds a busy loop inside the container. Failures carry their profile
  in the report (`[cpu1]`); a capped run costs its cap (one or half a CPU), so
  it still runs while runner jobs are busy, and stays constrained on purpose:
  the `node_compat` harness scales its per-test timeouts by the CPU each running
  test gets (factor 2 at `cpu1`, 4 at `cpu0.5` with two tests at once, 1 in the
  gate's container), so a test that is only slow there no longer times out,
  while a hang still does. (A capped container's `navigator.hardwareConcurrency`
  follows the cap while `os.cpus()` still lists the host's CPUs, so a test
  comparing the two is expected to fail there.)
- **Reports** every test that failed at least once: in how many repetitions, how
  (`failed`; `retried`: the harness's CI retry passed it, which a gate run
  survives but which is still a flake; `crashed`: a test binary died without
  naming a test; `timeout`; `git-dirty`), and the first failure's output
  (`report.md` and `report.json` in each host's run directory, every log under
  `logs/`). Exit 0 when clean, 1 on any flake, 2 on an infrastructure error.

`--host a:w,b:w` splits every unit's repetitions among the hosts by weight
(`local` is the machine that runs the command); with one host it all runs there.
biscuits covers Linux x64, the mini's Docker Linux arm64; macOS and Windows
flakes are out of reach (`--native` runs a macOS share on the host under
`nice -n 19`, one run at a time, but unsigned test binaries that listen can
raise the firewall's dialog: never on a machine someone is using).

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

**Carry-forward.** A re-hunt never retests what an earlier hunt vetted. By
default (`--carry auto`) a run carries from the newest finished run (every host
done, exit 0 or 1, every host's share present) whose candidate is an ancestor of
the new one; `--carry <run-id>` names it, `--no-carry` runs everything. With
`--host`, the machine that starts the hunt finds that run on the hosts (and in
its own `~/flake-hunt/runs`), merges their results and hands each host
`runs/<id>/carry.json`. When the earlier run qualified for the marker and no
`--mode` or `--suite` is given, its selection is adopted, so re-running the bare
command after a fix keeps the marker's coverage. Each host then builds the
selection as usual and drops every unit (a lib test binary, a test file of
`unit` / `unit_node`, a shard, a filtered run; `units` in `report.json`) that:

- ran in the earlier run (or was carried by it) with at least as many
  repetitions, and the earlier run had the same profiles and at least the same
  `--constrained-reps`;
- finished with no failure, or with failures that all name a test (`failed`,
  `retried`): those tests then run on their own at `--flaky-reps` and the rest
  of the unit carries. A crash, a timeout or a dirty tree reruns the whole unit;
- is not affected by the diff from the commit its result came from (a result
  carried twice keeps its first commit) to the candidate.

The diff rule, per changed file (`classifyFile` / `unitAffected` in the script):

- **Nothing**: Markdown at the top level, `doc/`, `.github/` Markdown and issue
  templates, workflows other than `denext_tests*.yml`, `tools/*.md`, the hunt
  itself and the lint / format scripts, `.dprint.json`, `.gitignore`. A change
  that is formatting only (whitespace between tokens and trailing commas outside
  string literals, in Rust, JS/TS and JSON outside `tests/` and `testdata/`:
  what rustfmt and dprint change) counts as no change. Markdown a crate holds
  counts as the crate's (a `build.rs` reads one).
- **One test**: `tests/unit/<x>_test.ts` (and its snapshot) the units of
  `unit::<x>_test`; a `tests/unit_node/` test file its file; a `tests/specs/`
  file inside a spec directory that spec; `tests/integration/<x>_tests.rs` that
  module. A unit filtered to other tests keeps its result; a carried shard keeps
  its other tests' results and the touched test runs on its own.
- **One suite**: any other file under `tests/unit/`, `tests/unit_node/`,
  `tests/specs/`, `tests/integration/`, and anything under `tests/node_compat/`.
- **A crate and everything built from it**: a file inside a workspace crate
  (Rust, JS, `Cargo.toml`, `build.rs`, fixtures). The crate and every crate that
  depends on it (any kind of dependency, from `cargo metadata`) lose their lib
  test results; when one of them is something the harness suites run or are
  built from (`deno`, `denort`, `test_server`, the crates under `tests/` and all
  they depend on), every harness unit loses its result too. In practice a change
  under `libs/`, `ext/`, `runtime/` or `cli/` reruns every harness suite and the
  lib tests of the crates above it, while the lib tests of unrelated crates
  carry.
- **Everything**: `Cargo.lock`, the root `Cargo.toml`, `rust-toolchain.toml`,
  `denext_tests_os.yml` (the test arguments), the rest of `.github/` (the
  prebuilt V8 pin), shared test inputs (`tests/testdata/`, `tests/util/`,
  `tests/registry/`, …) and any path in no crate.

**The release marker.** A clean run that is full, or fast with at least
`--suite libs,unit`, writes `~/.denext-runners/flake-hunt/<full sha>.ok` on the
machine that started it (with `--host biscuits`: the Mac mini), a JSON record of
the mode, suites, repetitions, hosts and times. A run removes that commit's old
marker when it starts and writes nothing when it finds a flake. A run that
carried writes the marker for its own candidate when its own part is clean; the
record then also holds `carry`: `from` (the runs results came from), `ran` (the
units run here) and `units` (each carried unit's label with the `runId` and
`sha` its result came from), so every unit behind the marker names the run that
vetted it. On the mini a hook refuses to dispatch the `denext tests` workflow
unless the marker of the exact commit the ref points at exists
(`denext-internal` `ops/release/gate-guard-hook.py`).

## denext/targeted_test.ts

A targeted `denext tests` run (DENEXT.md item 496): only the named tests,
filtered, on the named OS legs, after each leg's debug build. It is for checking
one fix in minutes instead of the full ~3 h matrix, and never for a release: the
tag gate and the verified-SHA guard leave a targeted run out, so it vouches for
nothing and a red one blocks nothing.

```sh
# push HEAD to a throwaway pretest/targeted-<sha12>-<time> branch and dispatch
deno run --allow-run=git,gh tools/denext/targeted_test.ts --os linux,windows \
  specs:kernel_sandbox::policy::config unit:pty_test
# another commit, every leg, a lib test; print the commands only
deno run --allow-run=git,gh tools/denext/targeted_test.ts --commit fix/x --dry-run \
  --os linux,linux-arm,macos,macos-x64,windows,windows-arm \
  libs:deno_permissions:kernel_broker::net
# the same by hand
git push origin <sha>:refs/heads/pretest/<name>
gh workflow run denext_tests.yml -R Brainwires/deno --ref pretest/<name> \
  -f only='specs:kernel_sandbox::policy::config unit:pty_test' -f os=linux,windows
```

- **Items** (`only`, separated by spaces or newlines), each
  `<suite>[:<filter>]`: `specs`, `unit`, `unit_node`, `integration` and
  `node_compat` run `cargo test -p <suite>_tests --test <suite> -- <filter>` in
  one job per item (unsharded; the harness runs every test whose name contains
  the filter, and with no filter the whole suite). For `node_compat`, a filter
  also selects tests `config.jsonc` does not enable. `libs:<crate>[:<filter>]`
  runs `cargo test --locked --lib -p <crate> -p deno_v8 [-- <filter>]`
  (`deno_core`, `serde_v8` and `deno_ops` without `deno_v8`). All lib items run
  in the leg's one `test libs` job, one after another. The crate must be one the
  full run's lib steps test. A filter takes letters, digits and
  `_ . : / @ + = -` (not first).
- **Legs** (`os`, comma-separated, no spaces): `linux` (ubuntu-22.04),
  `linux-arm`, `macos` (macos-15), `macos-x64`, `windows` (windows-2022) and
  `windows-arm`. Any suite runs on any leg here, including the ones the full run
  tests on Linux alone.
- **Skipped**: lint, cargo audit, every suite and shard not named, the macOS
  LaunchServices test, and the webrtc mDNS test. The `full`, `macos` and
  `windows` inputs are ignored; `macos_runner` still applies.
- **Where**: a `pretest/*` branch, not the release candidate's own branch. The
  gate ignores the run either way, but this keeps the candidate's run list the
  release's. No workflow runs on a push to `pretest/*`, and the Windows pools
  serve it. Delete the branch afterwards (`git push origin --delete <branch>`).
- `.github/denext-runtime/targeted_tests.py` holds the syntax and the plan each
  leg runs. `targeted_tests_test.py` checks them against the workflows.
