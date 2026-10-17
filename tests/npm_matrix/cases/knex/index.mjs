import knex from "knex";
const k = knex({ client: "pg", connection: "postgres://postgres:pw@127.0.0.1:55432/postgres" });
await k.schema.dropTableIfExists("kx"); await k.schema.createTable("kx", (t) => { t.increments(); t.string("n"); });
await k("kx").insert({ n: "a" }); const r = await k("kx").select(); await k.schema.dropTable("kx"); await k.destroy();
if (r[0].n !== "a") throw new Error("knex"); console.log("SMOKE_OK");
