import db, { name, query } from "./db.ts";

export function run(): string {
  return `${query("x")}|${name}|${db.kind}`;
}
