import mongoose from "mongoose";
await mongoose.connect("mongodb://127.0.0.1:57017/ncp");
const M = mongoose.model("K", new mongoose.Schema({ n: Number }));
await M.create({ n: 3 }); const d = await M.findOne({ n: 3 });
await mongoose.connection.dropDatabase(); await mongoose.disconnect();
if (d.n !== 3) throw new Error("mongoose"); console.log("SMOKE_OK");
