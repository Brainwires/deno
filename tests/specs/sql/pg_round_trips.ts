// Round trips per transaction shape, through a proxy that delays every chunk
// (delay_proxy.ts). The spec prints the client's turns (requests it had to
// wait for an answer before sending); `--timing` (the benchmark) also prints
// the median wall time in round trips:
//
//   DENO_SQL_TEST_POSTGRES_URL=postgres://... deno run \
//     --allow-env --allow-net tests/specs/sql/pg_round_trips.ts --timing
import { startDelayProxy } from "./delay_proxy.ts";

const timing = Deno.args.includes("--timing");
// The benchmark uses 10 ms; the spec, which only counts turns, more slack.
const delayMs = timing ? 10 : 25;
const iterations = timing ? 20 : 1;

const target = new URL(Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!);
const proxy = startDelayProxy(
  target.hostname,
  Number(target.port || 5432),
  delayMs,
);
const url = new URL(target);
url.hostname = "127.0.0.1";
url.port = String(proxy.port);
// One connection, so every shape reuses the same warm one (and its prepared
// statements).
const sql = new Deno.SQL(url.href, { max: 1 });

const setup = (v: string) =>
  sql.unsafe("select set_config('app.v', $1, true)", [v]);

// Each case resolves to what it checks (printed once).
const cases: Array<[string, () => Promise<unknown>]> = [
  ["bare query", async () => (await sql`select 1 as x`)[0].x],
  [
    "begin, 1 query returned",
    async () => (await sql.begin((tx) => tx`select 1 as x`))[0].x,
  ],
  [
    "begin, 1 query awaited",
    async () => (await sql.begin(async (tx) => await tx`select 1 as x`))[0].x,
  ],
  [
    "begin, 2 queries returned (array)",
    async () => {
      const [a, b] = await sql.begin((tx) => [
        tx`select set_config('app.v', ${"one"}, true)`,
        tx`select current_setting('app.v', true) as v`,
      ]);
      return [a.length, b[0].v];
    },
  ],
  [
    "begin, 2 pipelined queries (RLS shape)",
    async () => {
      const result = await sql.begin(async (tx) => {
        const s = tx.unsafe("select set_config('app.v', $1, true)", ["rls"])
          .execute();
        const q = tx`select current_setting('app.v', true) as v`.execute();
        const [, rows] = await Promise.all([s, q]);
        return rows;
      });
      return result[0].v;
    },
  ],
  [
    "begin, 2 sequential awaited queries",
    async () =>
      (await sql.begin(async (tx) => {
        await tx`select set_config('app.v', ${"seq"}, true)`;
        return await tx`select current_setting('app.v', true) as v`;
      }))[0].v,
  ],
  [
    "failing statement, returned (rollback)",
    async () => {
      try {
        await sql.begin((tx) => tx`select 1/0 as x`);
        return "committed?";
      } catch (e) {
        return (e as Deno.PostgresError).code;
      }
    },
  ],
  [
    "failing statement, awaited (rollback)",
    async () => {
      try {
        await sql.begin(async (tx) => {
          await tx`select 1/0 as x`;
        });
        return "committed?";
      } catch (e) {
        return (e as Deno.PostgresError).code;
      }
    },
  ],
  [
    "savepoint, 1 query returned",
    async () =>
      (await sql.begin(async (tx) => {
        const rows = await tx.savepoint((sp) => sp`select 2 as x`);
        return rows;
      }))[0].x,
  ],
];

// Opens the connection and prepares every statement.
await setup("warm").catch(() => {});
for (const [, run] of cases) await run();

const lines: string[] = [];
for (const [name, run] of cases) {
  const times: number[] = [];
  let turns = 0;
  let result: unknown;
  for (let i = 0; i < iterations; i++) {
    proxy.reset();
    const t0 = performance.now();
    result = await run();
    times.push(performance.now() - t0);
    turns = proxy.turns;
  }
  times.sort((a, b) => a - b);
  const rt = times[times.length >> 1] / (2 * delayMs);
  lines.push(
    `${name}: ${turns} turn(s)` +
      (timing ? `, ${rt.toFixed(2)} RT` : "") +
      ` -> ${JSON.stringify(result)}`,
  );
}
for (const line of lines) console.log(line);

// Nothing leaked out of a transaction: the setting was local.
console.log(
  "after:",
  JSON.stringify((await sql`select current_setting('app.v', true) as v`)[0].v),
);

await sql.close();
await proxy.close();
