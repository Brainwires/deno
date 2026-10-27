// Copyright 2018-2026 the Deno authors. MIT license.
// denext runtime e2e: package one small app per feature area with the STOCK
// `deno desktop` CLI against the runtime under test, launch it, and assert
// on what it reports. Run by e2e.sh (which sets up the display on Linux):
//
//   deno run -A run.ts
//
// Env: TARGET BACKEND RUNTIME_DIR WORK_DIR [E2E_OUT] [E2E_AREAS=a,b]
//      [E2E_VARIANT=default] [E2E_NA_BASELINE=enforce|report]
//
// Every check is pass, fail or n/a; an n/a carries the reason the check
// cannot run on this runner (never a silent skip). The n/a checks must be
// the ones expected-na.json lists for <target>/<backend>/<E2E_VARIANT>: a
// new one fails the run (enforce: the default under GitHub Actions; report
// elsewhere, where a developer's host has other capabilities). Writes
// $E2E_OUT/e2e-<target>-<backend>.json and a table to $GITHUB_STEP_SUMMARY.
// Exits 1 when any check failed or an n/a was unexpected.

import {
  AreaReport,
  DIR,
  type Env,
  log,
  path,
  rm,
  short,
} from "./lib/runner.ts";
import { baselineKey, compareNa, type NaBaseline } from "./lib/na_baseline.ts";

const AREAS: Record<
  string,
  () => Promise<{ run: (env: Env, rep: AreaReport) => Promise<void> }>
> = {
  origin: () => import("./areas/origin.ts"),
  appid: () => import("./areas/appid.ts"),
  deeplink: () => import("./areas/deeplink.ts"),
  window: () => import("./areas/window.ts"),
  dnd: () => import("./areas/dnd.ts"),
  passkeys: () => import("./areas/passkeys.ts"),
  sld: () => import("./areas/sld.ts"),
  menus: () => import("./areas/menus.ts"),
  asmt: () => import("./areas/asmt.ts"),
  update: () => import("./areas/update.ts"),
  keyring: () => import("./areas/keyring.ts"),
  keyringpersist: () => import("./areas/keyringpersist.ts"),
  exit: () => import("./areas/exit.ts"),
};
const AREA_TIMEOUT_MS = 20 * 60_000;

const need = (k: string) => {
  const v = Deno.env.get(k);
  if (!v) throw new Error(`${k} is required`);
  return v;
};

const backend = need("BACKEND");
if (backend !== "webview" && backend !== "cef") {
  throw new Error(`BACKEND ${backend}`);
}
const env: Env = {
  target: need("TARGET"),
  backend,
  runtimeDir: need("RUNTIME_DIR"),
  workDir: need("WORK_DIR"),
  nonce: Array.from(
    crypto.getRandomValues(new Uint8Array(6)),
    (b) => String.fromCharCode(97 + (b % 26)),
  ).join(""),
};
const out = Deno.env.get("E2E_OUT") ?? path(env.workDir, "results");
const only = (Deno.env.get("E2E_AREAS") ?? "").split(",").map((s) => s.trim())
  .filter(Boolean);
const areas = only.length ? only : Object.keys(AREAS);
for (const a of areas) if (!AREAS[a]) throw new Error(`unknown area ${a}`);

await rm(path(env.workDir, "build"));
await Deno.mkdir(env.workDir, { recursive: true });
await Deno.mkdir(out, { recursive: true });
log(
  `e2e ${env.target} ${env.backend} (nonce ${env.nonce}): ${areas.join(", ")}`,
);

const reports: AreaReport[] = [];
for (const area of areas) {
  const rep = new AreaReport(area);
  reports.push(rep);
  log(`\n=== ${area}`);
  const t0 = Date.now();
  try {
    const mod = await AREAS[area]();
    let timer: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([
      mod.run(env, rep),
      new Promise((_, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new Error(`area timed out after ${AREA_TIMEOUT_MS / 60000} min`),
            ),
          AREA_TIMEOUT_MS,
        );
      }),
    ]).finally(() => clearTimeout(timer));
  } catch (e) {
    rep.check(
      "area ran to completion",
      false,
      e instanceof Error ? e.stack ?? e.message : String(e),
    );
  }
  if (rep.checks.length === 0) rep.check("area produced checks", false);
  log(
    `=== ${area}: ${count(rep, "pass")} pass, ${count(rep, "fail")} fail, ${
      count(rep, "n/a")
    } n/a (${Math.round((Date.now() - t0) / 1000)} s)`,
  );
  // The apps' own result files, for the uploaded logs.
  const keep = path(env.workDir, "logs", "results", area);
  await Deno.mkdir(keep, { recursive: true });
  for (const e of await Array.fromAsync(Deno.readDir(DIR)).catch(() => [])) {
    if (
      e.isFile &&
      (e.name.startsWith(`${area}-`) || e.name.startsWith(`${area}.`))
    ) {
      await Deno.copyFile(path(DIR, e.name), path(keep, e.name)).catch(
        () => {},
      );
    }
  }
  if (area === "update") {
    for (
      const e of await Array.fromAsync(Deno.readDir(path(DIR, "update"))).catch(
        () => [],
      )
    ) {
      if (e.isFile) {
        await Deno.copyFile(path(DIR, "update", e.name), path(keep, e.name))
          .catch(() => {});
      }
    }
  }
  // Keep the disk in check: CEF apps are large.
  await rm(path(env.workDir, "build", area));
  const builds = await Array.fromAsync(Deno.readDir(path(env.workDir, "build")))
    .catch(() => []);
  for (const e of builds) {
    if (e.name.startsWith(`${area}-`)) {
      await rm(path(env.workDir, "build", e.name));
    }
  }
}

function count(rep: AreaReport, s: string) {
  return rep.checks.filter((c) => c.status === s).length;
}

const failed = reports.flatMap((r) =>
  r.checks.filter((c) => c.status === "fail").map((c) => `${r.area}: ${c.name}`)
);

// The n/a baseline (P7).
const variant = Deno.env.get("E2E_VARIANT") || "default";
const naMode = Deno.env.get("E2E_NA_BASELINE") ??
  (Deno.env.get("GITHUB_ACTIONS") === "true" ? "enforce" : "report");
if (naMode !== "enforce" && naMode !== "report") {
  throw new Error(`E2E_NA_BASELINE: enforce or report, not ${naMode}`);
}
const baseline = JSON.parse(
  await Deno.readTextFile(new URL("./expected-na.json", import.meta.url)),
) as NaBaseline;
const na = compareNa(
  baseline,
  baselineKey(env.target, env.backend, variant),
  areas,
  reports.flatMap((r) =>
    r.checks.filter((c) => c.status === "n/a").map((c) => ({
      area: r.area,
      name: c.name,
      reason: c.reason,
    }))
  ),
);
const unexpectedNa = naMode === "enforce" ? na.unexpected : [];
const summary = {
  target: env.target,
  backend: env.backend,
  os: Deno.build.os,
  ok: failed.length === 0 && unexpectedNa.length === 0,
  naBaseline: {
    key: na.key,
    mode: naMode,
    unexpected: na.unexpected,
    noLongerNa: na.noLongerNa,
  },
  areas: Object.fromEntries(
    reports.map((r) => [r.area, { checks: r.checks, notes: r.notes }]),
  ),
};
const file = path(out, `e2e-${env.target}-${env.backend}.json`);
await Deno.writeTextFile(file, JSON.stringify(summary, null, 2));

const md: string[] = [
  `### e2e ${env.target} / ${env.backend}: ${
    failed.length === 0 && unexpectedNa.length === 0 ? "passed" : [
      failed.length ? `${failed.length} failed` : "",
      unexpectedNa.length ? `${unexpectedNa.length} unexpected n/a` : "",
    ].filter(Boolean).join(", ")
  }`,
  "",
  "| area | pass | fail | n/a |",
  "| --- | --- | --- | --- |",
  ...reports.map((r) =>
    `| ${r.area} | ${count(r, "pass")} | ${count(r, "fail")} | ${
      count(r, "n/a")
    } |`
  ),
  "",
];
const nas = reports.flatMap((r) =>
  r.checks.filter((c) => c.status === "n/a").map((c) =>
    `- ${r.area}: ${c.name} — ${c.reason}`
  )
);
if (nas.length) {
  md.push("<details><summary>n/a</summary>", "", ...nas, "", "</details>", "");
}
if (failed.length) {
  md.push("Failed:", "");
  for (const r of reports) {
    for (const c of r.checks.filter((c) => c.status === "fail")) {
      md.push(
        `- ${r.area}: ${c.name}${
          c.detail === undefined
            ? ""
            : ` — \`${
              short(c.detail, 300).replace(/`/g, "'").replace(/\n/g, " ")
            }\``
        }`,
      );
    }
  }
  md.push("");
}
if (na.unexpected.length) {
  md.push(
    `${
      naMode === "enforce" ? "Unexpected n/a (fails the run)" : "Unexpected n/a"
    }, not in expected-na.json under \`${na.key}\`:`,
    "",
    ...na.unexpected.map((c) => `- ${c.area}: ${c.name} — ${c.reason ?? ""}`),
    "",
  );
  if (naMode === "enforce") {
    for (const c of na.unexpected) {
      // deno-lint-ignore no-console
      console.log(
        `::error::e2e ${na.key}: ${c.area}: "${c.name}" is n/a but expected-na.json does not list it (${
          c.reason ?? "no reason"
        })`,
      );
    }
  }
}
if (na.noLongerNa.length) {
  md.push(
    `No longer n/a (drop them from expected-na.json under \`${na.key}\`):`,
    "",
    ...na.noLongerNa.map((c) => `- ${c.area}: ${c.name}`),
    "",
  );
  for (const c of na.noLongerNa) {
    // deno-lint-ignore no-console
    console.log(
      `::notice::e2e ${na.key}: ${c.area}: "${c.name}" ran this time; remove it from expected-na.json`,
    );
  }
}
const stepSummary = Deno.env.get("GITHUB_STEP_SUMMARY");
if (stepSummary) {
  await Deno.writeTextFile(stepSummary, md.join("\n") + "\n", { append: true });
}
log(`\n${md.join("\n")}\nresults: ${file}`);
Deno.exit(failed.length === 0 && unexpectedNa.length === 0 ? 0 : 1);
