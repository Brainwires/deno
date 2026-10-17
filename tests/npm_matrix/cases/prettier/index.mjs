import * as p from "prettier"; const o = await p.format("const a={b:1}", { parser: "babel" });
if (o !== "const a = { b: 1 };\n") throw new Error(JSON.stringify(o)); console.log("SMOKE_OK");
