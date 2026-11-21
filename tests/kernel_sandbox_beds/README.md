# Forced kernel sandbox beds

denext (DENEXT.md item 451). Before a default of the kernel sandbox changes,
whole suites run with every `deno run` / `deno serve` under
`--kernel-sandbox=enforce`, and what fails is a reviewed list, never a silent
skip.

- `DENO_INTERNAL_KERNEL_SANDBOX_FORCE=enforce`: each run keeps its own grant;
  `-A` runs are confined too (the whole grant, plus the seccomp rules).
- `DENO_INTERNAL_KERNEL_SANDBOX_FORCE=project`: an unscoped read or write grant
  is also narrowed, for the kernel only, to the workspace, the working directory
  and the temp folder. Deno's checks keep the run's own grant, so a kernel
  refusal past them shows what the implicit set lacks.

An explicit `off` still wins, unfixed runs (`--watch`, HMR, `denext dev`) are
left alone, and a child of a confined process inherits its parent's sandbox
rather than being forced again.

## Running a bed

```sh
cargo build --bin deno --bin test_server
DENO_INTERNAL_KERNEL_SANDBOX_FORCE=enforce \
  cargo test -p node_compat_tests --test node_compat 2>&1 | tee node_compat.log
deno run -A tests/kernel_sandbox_beds/compare.ts --force enforce \
  --suite node_compat --cargo-log node_compat.log
```

`compare.ts` takes `--cargo-log` (specs, node_compat), `--npm-jsonl` (the npm
matrix's `--json`) or `--deno-test-log` (denext's `deno test` suites), `--force`
(default `enforce`) and `--os` (default: this OS). In CI:
`.github/workflows/denext_kernel_beds.yml`, dispatched by hand.

## The expected failures

`expected_failures.jsonc` maps each forced mode (`enforce`, `project`), then
each suite, to test names (a trailing `*` is a prefix) with the OSes they fail
on (`linux`, `darwin`; default: every OS) and why. `compare.ts --force <mode>`
reads that mode's list. Each entry is one of:

- a test that checks unconfined behaviour on purpose (for example that `-A`
  leaves the kernel sandbox off), which the force changes by design;
- a gap the kernel sandbox documents (a subcommand it doesn't cover yet, a rule
  only the broker can hold);
- a missing implicit-set entry or a compatibility bug, with the item that fixes
  it.

A failure not on the list fails the bed. A listed entry that passed is reported
as stale: take it off.

## Running a bed on a shared machine

The specs suite starts the test server on fixed ports (4545, 4260, 5545, ...):
run one specs bed at a time per machine. On a desktop Mac, servers the suites
start listen on every interface, and macOS's firewall asks about each unsigned
debug binary: run the macOS beds on CI runners, not on a machine someone is
using. A home without a `~/.npmrc` (as on CI runners) keeps the confined runs'
"treated as absent" note out of every expected output.

## Stage beds and the soak

denext (DENEXT.md item 472). What `--kernel-sandbox=auto` applies is one
constant (`AUTO_STAGE` in `cli/lib/kernel_sandbox.rs`). Before it moves, the
suites run at the next stage with nothing forced:

```sh
DENO_INTERNAL_KERNEL_SANDBOX_STAGE=r4 \
  cargo test -p specs_tests --test specs 2>&1 | tee specs.log
deno run -A tests/kernel_sandbox_beds/compare.ts --force stage-r4 \
  --suite specs --cargo-log specs.log
```

`r3`: a compiled program and an explicit `auto` enforce, a run with no terminal
warns. `r4`: every run whose permissions are fixed enforces. The variable only
raises the stage, so it only narrows. A mode `auto` chose never refuses to
start: where enforce can't hold a grant the run falls back to warn, so a stage
bed's failures are behaviour changes, not startup refusals. Their lists are the
`stage-r3` / `stage-r4` keys of `expected_failures.jsonc`.

In CI: the workflow's `stage` input with `force: none`.

A stage flips only after its bed soaked: **seven green full runs in a row**
(every suite, macOS included), each started four hours or more after the one
before, all on the candidate commit or its ancestors, the newest on the
candidate itself. A full run that fails or is cancelled resets the count. The
runs are dispatched by hand (no schedule); the four-hour spacing keeps what
seven nights gave (a day at least of different times and runner states), and
seven dispatches at once count as one.

```sh
deno run -A tests/kernel_sandbox_beds/soak.ts --bed stage-r4 --candidate <sha>
```

prints the counted runs, what is missing and the `gh workflow run` line for the
next one; it exits 0 when the soak is complete. It reads the runs with `gh api`,
or, where `gh` is missing or not signed in, from GitHub's public API with no
token. `r4_preconditions.ts` checks the rest of what must hold before the R4
flip.

## The R4 flip (denext.30)

The R4 soak run, the one that counts:

```sh
gh workflow run denext_kernel_beds.yml --ref train/denext30 -f force=none \
  -f stage=r4 -f macos=true -f suites=specs,node_compat,npm_matrix,denext \
  -f denext_ref=<the denext 5.0 branch>
```

A triage run is the same with fewer suites or no macOS (it doesn't count, and
doesn't reset the count). Triage happens on the merged train before the flip
commit; each failure becomes a fix, a test that says `--kernel-sandbox=off`
itself (the harness has no global opt-out: a test that asserts unconfined
behaviour says so), or a `stage-r4` entry with a settled reason. Compiled
programs ignore the stage variable, so a compiled spec's mode is the one it
bakes.

The desktop bed is the `kernelsandbox` e2e area of `denext_runtime.yml`
(`-f e2e_areas=kernelsandbox`): a packaged app is a compiled program, so it runs
with strict baked whatever the stage.

```sh
deno run -A tests/kernel_sandbox_beds/r4_preconditions.ts --candidate <sha> \
  [--deno target/release/deno] [--denext ../denext]
```

exits 0 when every check passes or is pending:

- `2a`: no "to triage" / "to fix" entry in `enforce`, `project`, `stage-r4`;
- `2b-stage-r4`: the soak above (seven green full runs, four hours apart, on the
  candidate or its ancestors, the newest on the candidate; a failed or cancelled
  full run resets it);
- `2c-enforce`, `2c-project`: the forced beds green once on the candidate;
- `3`, `3b`: the startup cost with `--deno`: Mode A at most 30 ms, Mode B (the
  supervising broker, a host-scoped network grant) at most 60 ms added;
- `8` (replacing "R3 shipped in two releases", which can't hold): both stage
  beds soaked, then two 5.0 release candidates on this runtime without a stage
  revert. The second half can only come after the flip ships, so it reads
  `pending` (it gates 5.0.0 final, not the flip) until recorded.

`r4_evidence.json` records what a script can't check (shape:
`r4_evidence.schema.json`): each key is a check's id, each value where the
evidence is, as a string or `{ "evidence", "recorded", "by" }`. An entry turns a
`manual` or `pending` check into a pass, never a failed one; an entry for no
check fails the run.
