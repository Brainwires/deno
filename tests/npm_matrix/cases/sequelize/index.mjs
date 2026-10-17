import { Sequelize, DataTypes } from "sequelize";
const s = new Sequelize("postgres://postgres:pw@127.0.0.1:55432/postgres", { logging: false });
const U = s.define("sq_user", { name: DataTypes.STRING }); await U.sync({ force: true });
await U.create({ name: "q" }); const u = await U.findOne(); await U.drop(); await s.close();
if (u.name !== "q") throw new Error("sequelize"); console.log("SMOKE_OK");
