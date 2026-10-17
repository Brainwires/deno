# npm matrix

Real npm packages run by this fork's `deno`, each in its own `package.json`
project with a real `node_modules` and no `node` on `PATH`: frameworks (Express,
Fastify, Next.js, NestJS, ...), ORMs and database clients, native addons, test
runners, build tools, browsers, network clients, logging and cloud SDKs. It is
the regression net for Node.js compatibility that the `tests/node_compat` suite
can't be: the programs a Node user actually runs.

`.github/workflows/denext_npm_matrix.yml` runs it nightly, on demand and on a
push that changes it.

## A case

`cases/<name>/` is the project as it is copied into the work directory:

- `package.json`: its dependencies, pinned to exact versions so a change in the
  matrix's result is a change in Deno, not in the packages (transitive
  dependencies still float).
- the program, which prints `SMOKE_OK` when everything it checked worked.
- `case.json` (not copied):
  - `run`: the shell command, run after `deno install`, with only `deno` on
    `PATH` (`deno run`, `deno task`, `npm:` binaries).
  - `node`: the same program under Node.js, to tell a broken case from a Deno
    gap when investigating (the matrix does not run it).
  - `expect`: `pass`, `pass-with-allow-scripts` (works only once dependency
    install scripts are allowed) or `fail`.
  - `gap`: for a case that does not pass, the item of
    `plans/node-compat-gaps.md` (denext) it is waiting on.
  - `services`: `postgres`, `redis`, `mongo`, `minio`.

## Running it

```sh
tests/npm_matrix/services.sh up    # Docker: Postgres, Redis, MongoDB, MinIO
deno run -A --config=tests/config/deno.json tests/npm_matrix/run.ts \
  --deno target/debug/deno [--only express,prisma] [--skip puppeteer] \
  [--jobs 3] [--work-dir /tmp/m] [--json results.jsonl]
tests/npm_matrix/services.sh down
```

A case that fails is retried after `deno install --allow-scripts`. The run exits
1 when any case does worse than its `expect`, and lists the cases that do better
so their `expect` can be raised.

The programs come from the 2026-10-17 Node.js compatibility survey (54 programs,
about 68 packages); its write-up is denext's `plans/node-compat-gaps.md`.
