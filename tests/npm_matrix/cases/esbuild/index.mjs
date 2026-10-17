import * as esbuild from "esbuild";
const r = await esbuild.build({ entryPoints: ["in.ts"], bundle: true, write: false, format: "esm" });
const t = await esbuild.transform("let x: number = 1", { loader: "ts" });
if (!r.outputFiles[0].text.includes("a * 2") || !t.code.includes("let x = 1")) throw new Error("esbuild");
console.log("SMOKE_OK");
