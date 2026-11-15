# Kernel-sandbox escape corpus

The regression suite for kernel-enforced permissions: every way native code can
reach the OS under a fixed Deno grant, tried once per vector, each marked with
the guarantee-matrix cell it tests.

```sh
deno run -A corpus.ts                     # build, run, print, check (--expect=today)
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

## Kernel matrix

The same directory runs unchanged on any Linux kernel: copy it to a VM booted on
the kernel under test and run `corpus.ts` with that VM's `deno`; `--json` gives
one file per kernel to diff. Run it on 5.15, 6.1, 6.8 and 6.12 or newer
(Landlock ABI 1, 2, 4 and 6+), on x86_64 and aarch64.
