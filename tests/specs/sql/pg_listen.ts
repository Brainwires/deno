// LISTEN / NOTIFY: one dedicated connection for every channel, payloads up
// to the server's limit, delivery order, unlisten, notices, and a reconnect
// (with LISTEN again) after the server drops the listening connection.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const app = "denosql_listen_" + Date.now();
const sql = new Deno.SQL(url, {
  max: 2,
  connection: { application_name: app },
});
const watcher = new Deno.SQL(url, { max: 1 });
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until(check: () => boolean, label: string) {
  for (let i = 0; i < 200 && !check(); i++) await sleep(25);
  if (!check()) console.log("timed out:", label);
}
const listeners = async () =>
  (await watcher`select count(*)::int as n from pg_stat_activity
    where application_name = ${app} and query ilike 'listen%'`)[0].n;

// One channel, two handlers, onlisten.
const a: string[] = [];
const b: string[] = [];
let listened = 0;
const subA = await sql.listen("news", (p) => a.push(p), () => listened++);
const subB = await sql.listen("news", (p, info) => {
  b.push(`${info.channel}:${p}:${typeof info.pid}`);
});
console.log("onlisten:", listened);
await sql.notify("news", "first");
await sql`select pg_notify('news', 'second')`;
await until(() => a.length === 2 && b.length === 2, "two handlers");
console.log("handler a:", a);
console.log("handler b:", b);

// Many channels share the one listening connection.
const seen = new Set<string>();
const subs = [];
for (let i = 0; i < 50; i++) {
  subs.push(await sql.listen(`ch_${i}`, (p) => seen.add(p)));
}
for (let i = 0; i < 50; i++) await sql.notify(`ch_${i}`, `p${i}`);
await until(() => seen.size === 50, "50 channels");
console.log(
  "50 channels:",
  seen.size,
  "listening connections:",
  await listeners(),
);
await Promise.all(subs.map((s) => s.unlisten()));

// Channel names are exact (quoted): case and punctuation are kept.
const odd: string[] = [];
const subOdd = await sql.listen('My "Odd" Chan', (p) => odd.push(p));
await sql.notify('My "Odd" Chan', "yes");
await sql.notify("my odd chan", "no");
await until(() => odd.length === 1, "odd channel");
console.log("odd channel:", odd);
await subOdd.unlisten();

// The largest payload the server accepts (under 8000 bytes), in order.
const big: string[] = [];
const subBig = await sql.listen("big", (p) => big.push(p));
const payload = "x".repeat(7999);
await sql.notify("big", payload);
try {
  await sql.notify("big", "y".repeat(8000));
} catch (e) {
  console.log("payload too long:", (e as Deno.PostgresError).code);
}
for (let i = 0; i < 100; i++) await sql.notify("big", String(i));
await until(() => big.length === 101, "ordering");
console.log(
  "7999 bytes:",
  big[0] === payload,
  "in order:",
  big.slice(1).every((p, i) => p === String(i)),
);
// Payloads sent in one transaction arrive at commit, in order.
await sql.begin(async (tx) => {
  for (let i = 0; i < 3; i++) await tx`select pg_notify('big', ${"tx" + i})`;
});
await until(() => big.length === 104, "transaction");
console.log("transaction:", big.slice(101));
await subBig.unlisten();

// unlisten: no more deliveries for that handler.
await subA.unlisten();
await sql.notify("news", "third");
await until(() => b.length === 3, "after unlisten");
console.log("after unlisten:", a.length, b.length);

// The server drops the listening connection: it reconnects, LISTENs again
// and calls onlisten again.
let relistened = 0;
const lost: string[] = [];
const subLost = await sql.listen(
  "lost",
  (p) => lost.push(p),
  () => relistened++,
);
const [{ pid }] = await watcher`select pid from pg_stat_activity
  where application_name = ${app} and query ilike 'listen%' limit 1`;
await watcher`select pg_terminate_backend(${pid})`;
await until(() => relistened === 2, "relisten");
await sql.notify("lost", "after reconnect");
await sql.notify("news", "news after reconnect");
await until(() => lost.length === 1 && b.length === 4, "after reconnect");
console.log("reconnected:", relistened, lost, b[3]);
await subLost.unlisten();
await subB.unlisten();
// The listening connection closes once nothing listens.
let left = await listeners();
for (let i = 0; i < 80 && left > 0; i++) {
  await sleep(25);
  left = await listeners();
}
console.log("listening connections after unlisten:", left);

// Notices go to onnotice (pooled connections and the listener).
const notices: string[] = [];
const noisy = new Deno.SQL(url, {
  max: 1,
  onnotice: (n) => notices.push(`${n.severity} ${n.code} ${n.message}`),
});
await noisy`do $$ begin raise notice 'hello %', 42; raise warning 'careful'; end $$`;
await until(() => notices.length === 2, "notices");
console.log("notices:", notices);
await noisy.close();

// A handler that throws does not stop the others.
const after: string[] = [];
globalThis.addEventListener("error", (e) => {
  e.preventDefault();
  console.log("reported:", e.error.message);
}, { once: true });
const subThrow = await sql.listen("throws", () => {
  throw new Error("handler failed");
});
const subAfter = await sql.listen("throws", (p) => after.push(p));
await sql.notify("throws", "still delivered");
await until(() => after.length === 1, "throwing handler");
console.log("after a throwing handler:", after);
await subThrow.unlisten();
await subAfter.unlisten();

// close() ends listening; listen() after close() rejects.
await sql.listen("closing", () => {});
await sql.close();
try {
  await sql.listen("closing", () => {});
} catch (e) {
  console.log("after close:", (e as Deno.PostgresError).code);
}
await watcher.close();
