import jwt from "jsonwebtoken"; import { generateKeyPairSync } from "node:crypto";
const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 }); const t = jwt.sign({ a: 1 }, privateKey, { algorithm: "RS256" });
const e = jwt.sign({ a: 2 }, "s", { expiresIn: 60 }); if (jwt.verify(t, publicKey).a !== 1 || jwt.verify(e, "s").a !== 2) throw 0; console.log("SMOKE_OK");
