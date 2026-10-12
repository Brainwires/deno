const sql = new Deno.SQL("sqlite://ro.db");
await sql`create table t (a)`;
await sql`insert into t values (1)`;
await sql.close();
