import argon2 from "argon2";
const h = await argon2.hash("pw"); if (!(await argon2.verify(h, "pw"))) throw new Error("argon2"); console.log("SMOKE_OK");
