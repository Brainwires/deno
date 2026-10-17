import bcrypt from "bcrypt";
const h = await bcrypt.hash("pw", 8); if (!(await bcrypt.compare("pw", h)) || bcrypt.compareSync("no", h)) throw new Error("bcrypt");
console.log("SMOKE_OK");
