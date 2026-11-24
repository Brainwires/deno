# Kernel-sandbox escape corpus

The regression suite for kernel-enforced permissions: every way native code can
reach the OS under a fixed Deno grant, tried once per vector, each marked with
the guarantee-matrix cell it tests.

```sh
deno run -A corpus.ts                     # build, run, print, check (--expect=today)
deno run -A corpus.ts --expect=enforce    # under --kernel-sandbox=enforce
deno run -A corpus.ts --json=out.json     # also write every row as JSON
deno run -A corpus.ts --vectors=ffi,child # a subset of the vectors
deno run -A corpus.ts --dir=/tmp/kc       # keep the work tree
```

Needs a C compiler (`cc`, or `$CC`). Unix only for now.

## What runs

- `probe.c`, built three ways: the `--allow-run` child (`probe`), the FFI
  library (`libprobe.so` / `.dylib`) and a Node-API addon (`probe.node`, also
  installed as `node_modules/corpus-native`, the native-npm-package vector).
- `corpus.ts` (unconfined) lays out `outside/` (a secret, files to overwrite)
  and `work/` (the only paths the grant names), starts the listeners the network
  probes aim at (`probe serve`, outside the confined tree), and runs `host.ts`
  once per vector under

  ```
  --no-prompt --allow-read=<work> --allow-write=<work> --allow-ffi=<work>
  --allow-run=<work>/probe --allow-net=example.com:<port>
  ```

  with fd 3 open on a file outside the grant (an fd inherited from the launcher)
  and a variable `--allow-env` does not grant.
- `host.ts` (confined) only loads the probe and calls it; every attempt is
  native code.
- `probes.ts` is the data: the probes, the matrix cell each one tests, and per
  cell the target for each platform column (macOS, Linux by Landlock ABI,
  Windows small tier): `kernel`, `coarser`, `deno-only`, `by-design`.

## States

| state   | meaning                                                                                               |
| ------- | ----------------------------------------------------------------------------------------------------- |
| `open`  | the operation reached its target; no layer refused it                                                 |
| `held`  | refused with `EPERM` / `EACCES`                                                                       |
| `host`  | refused by a host policy that is not ours (Yama, a sysctl, missing capabilities); the detail names it |
| `n/a`   | not applicable here (platform, missing target, kernel without the feature)                            |
| `crash` | the host process died before reporting this probe                                                     |

Before the confined runs, `corpus.ts` runs every probe once unconfined (the bare
executable, under whatever the host itself enforces: a container's seccomp
profile, a prohibit route, an outer sandbox, Yama). A confined refusal is `held`
only when that baseline was not refused; otherwise it is `host` (`classify.ts`,
tested by `classify_test.ts`). The first output line lists what the host refuses
on its own.

A network probe that was permitted but got no answer (timeout, unreachable) is
`open`: the matrix is about what the sandbox allows, not what the network
delivers.

## Expectations

`--expect=today` (the default, and what the spec test runs): with no kernel
enforcement nothing may be `held`. A held probe means an outer sandbox already
confines the process, or a probe is wrong. Each enforcement phase adds an
expectation derived from `probes.ts` and diffs the output against it; a probe
that is `open` where its cell says `kernel` is an escape.

`--expect=enforce` runs host.ts under `--kernel-sandbox=enforce` in the
configurations the kernel sandbox can hold (a name in `--allow-net` needs the
broker, which isn't built yet): `files` (`--allow-read/write/ffi=<work>`, no
network, no run), `net-all` (the same plus `--allow-net`), `run` (plus
`--allow-run=<work>/probe`, the child vector) and `nested` (a run list that
names the runtime and the launchers too). Under `--expect=enforce` a fifth
vector, `nested`, runs: the confined host starts `deno run -A host.ts ffi`,
which inherits the host's grant (the policy handed down) and drives the FFI
vector; its checks are expected as for a run list, and it must run where
`nested` lets it start. Two more vectors start that deno through a launcher that
drops the policy's carrier: `nested-env` through `env -i` (the variable cleared,
the descriptor still open: the nested deno finds it again, so it is expected as
`nested`) and `nested-closed` through a shell that closes the descriptor (the
nested deno knows a kernel sandbox of the runtime holds it, by its marker, but
not the grant: it refuses every permission, so every check is `n/a`, and each
must say `grants unknown`). `expect.ts` derives what each check may be from this
machine's column (macOS, or Linux by Landlock ABI):

| cell                                                   | macOS  | Linux ABI 1 | ABI 2-3 | ABI 4-5 | ABI 6+ |
| ------------------------------------------------------ | ------ | ----------- | ------- | ------- | ------ |
| fs.read, fs.write, fs.link, fs.symlink                 | held   | held        | held    | held    | held   |
| fs.proc, proc.ptrace, sys.surface, sys.userns          | n/a    | held        | held    | held    | held   |
| fs.meta (path forms refused under enforce)             | held   | held        | held    | held    | held   |
| network cells, `files` / `run` (no network)            | held   | held        | held    | held    | held   |
| IP cells, `net-all`                                    | open   | open        | open    | open    | open   |
| net.unix, net.unix-abstract, net.netlink, ipc.bus      | held   | held        | held    | held    | held   |
| exec.run-list, no run (in-process vectors)             | held   | held        | held    | held    | held   |
| exec.run-list, `run`                                   | either | either      | either  | either  | either |
| proc.signal                                            | held   | either      | either  | either  | held   |
| env (Linux scrubs the re-executed image's environment) | either | held        | held    | held    | held   |
| ipc.launch                                             | held   | n/a         | n/a     | n/a     | n/a    |
| fd.inherited, sys.info                                 | either | either      | either  | either  | either |

"held" also accepts `host` and `n/a`; "either" means the column documents the
cell as coarser, Deno-only or by design. Where Linux can't hold a configuration
exactly (`--kernel-sandbox=enforce` refuses to start), its checks come back as
`crash` and the run fails: that is a broken expectation, not a pass. Without
Landlock (or without an enforcer for the OS) the run is skipped with the reason,
except under `CI` (unless `DENO_TEST_NO_KERNEL_ENFORCER` names why the machine
has none), where it fails: the spec runner (`tests/specs/mod.rs`) requires an
enforcer, port rules and the broker's network side on Linux and macOS CI legs,
and reports `corpus_enforce` as ignored, not passed, where there is none.
`ENOSYS` from `io_uring_setup` and `clone3` counts as held (the kernel sandbox
answers so on purpose); the baseline turns it into `host` on a kernel that lacks
them.

Where the runtime's supervising broker holds the network (`deno info --json` →
`kernelSandbox.brokerNetBuilt`), two more configurations run: `net-host`
(`--allow-net=example.com:<port>,localhost:<port>`, a host-scoped grant) and
`net-host-run` (the same with `--allow-run=<work>/probe`, the child vector).
Every IP cell must be held in both, on every column: the runtime's own sockets
come from the broker, native code can't make one, a child's `connect` reaches
only the broker's proxy, and a socket the broker handed over can't be pointed at
another address (`net.retarget`: the host connects to the granted listener
through the broker first, so the probes hold such a socket). Without the broker
the run says the two were skipped.

The deny ceiling (`deny-ceiling`, under `--expect=enforce` too):
`-A
--deny-read=<outside> --deny-write=<outside>` with no `--kernel-sandbox`,
`auto` at R4 (`DENO_INTERNAL_KERNEL_SANDBOX_STAGE=r4`). Only the denies are
enforced and nothing else changes (`expectDenyCeiling`): on macOS (an
allow-default Seatbelt profile with the denies) `fs.read`, `fs.write`, `fs.meta`
and `fs.symlink` must be held, `fs.link`, `fs.proc`, `proc.ptrace`,
`fd.inherited` and `sys.info` may go either way, and every other cell must be
open as granted (a held network, exec, IPC, signal or environment cell means the
ceiling changed something else). On Linux the denies are Deno's checks only
until an overmount exists (decision O4), so every cell must be as without a
sandbox (a held one means a seccomp list or no-new-privileges was applied). The
nested vectors don't run in it.

## Kernel matrix

The same directory runs unchanged on any Linux kernel: copy it to a VM booted on
the kernel under test and run `corpus.ts` with that VM's `deno`; `--json` gives
one file per kernel to diff. Run it on 5.15, 6.1, 6.8 and 6.12 or newer
(Landlock ABI 1, 2, 4 and 6+), on x86_64 and aarch64.

With the lab's kernel matrix (one run at a time, aarch64 VMs; 5.15 / 6.1 / 6.8 /
6.12 / 7.3 = Landlock ABI 1 / 2 / 4 / 6 / 11), `<dir>` holding an aarch64 `deno`
and a copy of this folder as `escape_corpus/` (the lab copies `<dir>` to
`~/kmatrix-run/` in the guest):

```sh
lab kmatrix run --kernels all --cmd 'cd ~/kmatrix-run && ./deno run -A escape_corpus/corpus.ts --expect=enforce --json=corpus-$(uname -r).json' <dir>
```
