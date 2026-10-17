const db: Deno.SQL = new Deno.SQL("sqlite::memory:");
const rows: Deno.SQLResult<{ n: number }> = await db`select 1 as n`;
console.log(rows[0].n, typeof Deno.sql);
await db.close();
