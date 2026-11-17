# Forced kernel sandbox beds

denext (DENEXT.md item 448). Before a default of the kernel sandbox changes,
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
