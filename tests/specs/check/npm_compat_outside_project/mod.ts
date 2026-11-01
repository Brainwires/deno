import { readFileSync } from "node:fs";

export const text: string = readFileSync("mod.ts", "utf8");
