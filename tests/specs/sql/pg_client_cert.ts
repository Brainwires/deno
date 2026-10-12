// Client certificates (sslcert / sslkey, or tls: { cert, key }) for a role
// the server authenticates by certificate.
const url = new URL(Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!);
const caPath = Deno.env.get("DENO_SQL_TEST_POSTGRES_CA")!;
const certPath = Deno.env.get("DENO_SQL_TEST_POSTGRES_CLIENT_CERT")!;
const keyPath = Deno.env.get("DENO_SQL_TEST_POSTGRES_CLIENT_KEY")!;
const base = `postgres://certuser@localhost:${url.port}/postgres`;

async function check(label: string, sql: Deno.SQL) {
  try {
    const [row] = await sql`select current_user as who,
      (select ssl from pg_stat_ssl where pid = pg_backend_pid()) as ssl`;
    console.log(label, row);
  } catch (e) {
    console.log(label, "error:", (e as Deno.PostgresError).code);
  } finally {
    await sql.close();
  }
}

await check(
  "url files:",
  new Deno.SQL(
    `${base}?sslmode=verify-full&sslrootcert=${
      encodeURIComponent(caPath)
    }&sslcert=${encodeURIComponent(certPath)}&sslkey=${
      encodeURIComponent(keyPath)
    }`,
  ),
);
await check(
  "tls options:",
  new Deno.SQL(base, {
    tls: {
      ca: Deno.readTextFileSync(caPath),
      cert: Deno.readTextFileSync(certPath),
      key: Deno.readTextFileSync(keyPath),
    },
  }),
);
await check(
  "no certificate:",
  new Deno.SQL(base, { tls: { ca: Deno.readTextFileSync(caPath) } }),
);
const half = new Deno.SQL(base, {
  tls: {
    ca: Deno.readTextFileSync(caPath),
    cert: Deno.readTextFileSync(certPath),
  },
});
try {
  await half`select 1`;
} catch (e) {
  console.log("cert without key:", (e as Error).name);
}
await half.close();
