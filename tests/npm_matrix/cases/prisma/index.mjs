import { PrismaClient } from "@prisma/client";
const p = new PrismaClient();
await p.user.deleteMany(); await p.user.create({ data: { name: "a" } });
const n = await p.user.count(); await p.$disconnect();
if (n !== 1) throw new Error("count " + n); console.log("SMOKE_OK");
