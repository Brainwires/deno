# tsc

Deno type-checks with one of two engines (DENEXT.md item 43):

- **native** (the default): the native TypeScript 7 compiler (`tsgo`), run as a
  separate process. It is never downloaded: denext's `deno-cli-*` archives ship
  it in a `typescript-check/` directory next to `deno` (the binary plus the
  `lib.*.d.ts` files it loads from its own directory), the `check` role of
  `typescript_pins.json` (see `native_pin.rs`; fetched for the archives by
  `.github/denext-runtime/fetch_typescript.py`). `native.rs` finds it
  (`DENO_TSGO_BIN`, then `typescript-check/` beside the executable, then
  denext's toolchain cache), runs `tsc --pretty true` and maps its diagnostics
  (with their related information) back onto Deno's specifiers. The compiler
  sees a stock project: `tsconfig_gen.rs` and
  `cli/tools/installer/npm_compat.rs` (the `deno sync-types` work) write
  `.deno/tsconfig.json`, Deno's own types as a private `@types/deno` under
  `.deno/types/deno/`, and mirrors of the `jsr:` / `http(s):` / `npm:` types the
  project uses. `cli/tools/check.rs` (`run_native_check`) drives it, behind
  Deno's check cache, for `deno check` and every command that type-checks
  through `TypeChecker::check_graph`.
- **tsc** (`--check-engine=tsc`, `DENO_CHECK_ENGINE=tsc`): the in-process
  TypeScript 6.0.3 in `00_typescript.js` with the compiler host in
  `97_ts_host.js`, snapshotted into the CLI (`mod.rs`, `js.rs`).
  `deno check
  --doc` / `--doc-only` always use it, and so does `deno lsp`
  (`98_lsp.js`) unless its unstable tsgo backend is on (DENEXT.md item 42).

`dts/` holds Deno's own libs (`lib.deno.*.d.ts`) and TypeScript's
(`lib.es*.d.ts`, `lib.dom*.d.ts`) for the in-process engine; the native engine
writes Deno's into the generated `@types/deno` and uses the compiler's own
`lib.es*` / `lib.dom*`.

## Upgrading the native compiler

1. `tools/update_typescript_pin.ts check <version>` (the only edit: the Rust
   code, the release archive and the test harness all read the pins file).
2. Run the `check` spec tests (the harness fetches the newly pinned binary into
   `target/` on first use).

## Upgrading the in-process TypeScript

`00_typescript.js` is stock `typescript` from npm (no patches since
denoland/deno#35642): copy the release's `lib/typescript.js` over it and its
`lib/lib.*.d.ts` into `dts/`, then follow `tools/update_typescript.md` for the
rest of the checklist.
