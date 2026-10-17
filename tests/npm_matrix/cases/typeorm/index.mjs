import "reflect-metadata"; import { DataSource, EntitySchema } from "typeorm";
const U = new EntitySchema({ name: "to_user", columns: { id: { type: Number, primary: true, generated: true }, name: { type: String } } });
const ds = new DataSource({ type: "postgres", url: "postgres://postgres:pw@127.0.0.1:55432/postgres", entities: [U], synchronize: true, dropSchema: true });
await ds.initialize(); const r = ds.getRepository("to_user");
await r.save({ name: "t" }); const all = await r.find(); await ds.dropDatabase(); await ds.destroy();
if (all[0].name !== "t") throw new Error("typeorm"); console.log("SMOKE_OK");
