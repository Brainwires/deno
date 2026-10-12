// TLS modes against a server with a self-signed certificate for localhost.
const url = new URL(Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!);
const ca = Deno.readTextFileSync(Deno.env.get("DENO_SQL_TEST_POSTGRES_CA")!);

async function attempt(
  label: string,
  sqlUrl: string,
  options?: Deno.SQLOptions,
) {
  const sql = new Deno.SQL(sqlUrl, options);
  try {
    const [row] =
      await sql`select ssl from pg_stat_ssl where pid = pg_backend_pid()`;
    console.log(label, "ssl:", row.ssl);
  } catch (e) {
    console.log(label, "error:", (e as Deno.PostgresError).code);
  } finally {
    await sql.close();
  }
}

const withMode = (mode: string) => {
  const u = new URL(url);
  u.searchParams.set("sslmode", mode);
  return u.href;
};
await attempt("disable", withMode("disable"));
await attempt("prefer", withMode("prefer"));
await attempt("require", withMode("require"));
await attempt("verify-full (untrusted)", withMode("verify-full"));
await attempt("tls: true (untrusted)", url.href, { tls: true });
await attempt("verify-full + ca", url.href, { tls: { ca } });
await attempt("verify-full + ca, wrong name", url.href, {
  tls: { ca, servername: "example.com" },
});
await attempt("verify-ca + ca, wrong name", withMode("verify-ca"), {
  tls: { ca, servername: "example.com" },
});
await attempt("verify-ca (untrusted)", withMode("verify-ca"));
await attempt("rejectUnauthorized: false", url.href, {
  tls: { rejectUnauthorized: false },
});
