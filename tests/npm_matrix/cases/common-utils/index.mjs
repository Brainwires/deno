import _ from "lodash"; import dotenv from "dotenv"; import { Command } from "commander"; import chalk from "chalk"; import { v4, v7 } from "uuid"; import { z } from "zod";
import dayjs from "dayjs"; import yargs from "yargs"; import { hideBin } from "yargs/helpers"; import debug from "debug"; import fse from "fs-extra"; import { glob } from "glob"; import semver from "semver";
import spawn from "cross-spawn"; import nf from "node-fetch"; import ms from "ms";
// smoke.env, not .env: the repository ignores .env files
dotenv.config({ path: "smoke.env", quiet: true });
const bad = [];
if (_.chunk([1,2,3], 2).length !== 2) bad.push("lodash"); if (process.env.FOO !== "bar") bad.push("dotenv");
const c = new Command().option("-x <n>").parse(["node", "s", "-x", "3"]); if (c.opts().x !== "3") bad.push("commander");
if (!chalk.red("a")) bad.push("chalk"); if (v4().length !== 36 || v7().length !== 36) bad.push("uuid"); if (!z.object({ a: z.number() }).safeParse({ a: 1 }).success) bad.push("zod");
if (!dayjs("2020-01-01").isValid()) bad.push("dayjs"); if (yargs(hideBin(["n", "s", "--q=2"])).parseSync().q !== 2) bad.push("yargs");
debug("x")("y"); await fse.outputJson("o/a.json", { a: 1 }); if ((await fse.readJson("o/a.json")).a !== 1) bad.push("fs-extra");
if (!(await glob("*.mjs")).includes("index.mjs")) bad.push("glob"); if (!semver.satisfies("1.2.3", "^1")) bad.push("semver");
const r = spawn.sync("echo", ["cs"]); if (String(r.stdout).trim() !== "cs") bad.push("cross-spawn"); if (typeof nf !== "function") bad.push("node-fetch"); if (ms("1m") !== 60000) bad.push("ms");
if (bad.length) { console.error("FAILED:", bad); process.exit(1); } console.log("SMOKE_OK");
