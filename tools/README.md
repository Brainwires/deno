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
  privacy dialog. Each busy runner job on the host takes two workers' room, and
  a run waits while memory is under 4 GB or the disk under 35 GB.
- **Constrains the CPU** (`--profiles`, default `normal,cpu1,cpu0.5`): besides
  the gate's own container (`normal`, `--reps` times), every unit also runs
  `--constrained-reps` times (default 3) in the same container capped at
  `--cpus=1` and at `--cpus=0.5`, with the harness at the gate's own
  parallelism, so timing races that an idle many-core machine hides surface
  reliably. A `+hog` suffix (`cpu1+hog`), or `--hog` for every constrained
  profile, adds a busy loop inside the container. Failures carry their profile
  in the report (`[cpu1]`); a capped run counts half its CPUs against the worker
  budget, so it still runs while runner jobs are busy. (A capped container's
  `navigator.hardwareConcurrency` follows the cap while `os.cpus()` still lists
  the host's CPUs, so a test comparing the two is expected to fail there.)
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

**The release marker.** A clean run that is full, or fast with at least
`--suite libs,unit`, writes `~/.denext-runners/flake-hunt/<full sha>.ok` on the
machine that started it (with `--host biscuits`: the Mac mini), a JSON record of
the mode, suites, repetitions, hosts and times. A run removes that commit's old
marker when it starts and writes nothing when it finds a flake. On the mini a
hook refuses to dispatch the `denext tests` workflow unless the marker of the
exact commit the ref points at exists (`denext-internal`
`ops/release/gate-guard-hook.py`).
