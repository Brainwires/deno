// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Kernel-sandbox escape corpus: the orchestrator. Runs unconfined (-A).
//
//   deno run -A corpus.ts [--expect=today] [--json=<file>] [--dir=<d>] [--vectors=ffi,child]
//
// 1. Builds probe.c three ways with the system C compiler (`cc`, or $CC):
//    the child executable, the FFI library and the Node-API addon (also
//    installed as node_modules/corpus-native, the native-npm-package vector).
// 2. Lays out <dir>/outside (the secret, files to write over) and <dir>/work
//    (the only paths the grant names).
// 3. Starts `probe serve`: TCP v4 (an ungranted port and a port granted only
//    to a public name), TCP v6, UDP (also a fake DNS resolver), a pathname and
//    an abstract Unix socket. Its pid is the "process outside the tree".
// 4. Runs host.ts once per vector under the fixed grant
//      --no-prompt --allow-read=<work> --allow-write=<work> --allow-ffi=<work>
//      --allow-run=<work>/probe --allow-net=example.com:<granted port>
//    with fd 3 open on outside/secret2 (an fd inherited from the launcher) and
//    CORPUS_SECRET_ENV in the environment (not granted by --allow-env).
// 5. Prints one line per (probe, vector) with its matrix cell, and a summary.
//
// --expect=today (the default): with no kernel enforcement, nothing may be
// "held"; every probe must be open, n/a or host-blocked. The run fails if a
// probe is held, which means something already confines this process (an
// outer sandbox) or a probe is wrong. Later phases add expectations derived
// from probes.ts's matrix targets.

import { CELLS, PROBES, type Vector, VECTORS } from "./probes.ts";

const args = Object.fromEntries(
  Deno.args.map((a) => a.replace(/^--/, "").split("=")).map((
    [k, ...v],
  ) => [k, v.join("=") || "1"]),
);
const expect = args.expect ?? "today";
const here = new URL(".", import.meta.url).pathname;
const dir = Deno.realPathSync(
  args.dir ?? Deno.makeTempDirSync({ prefix: "kernel-corpus-" }),
);
const work = `${dir}/work`;
const out = `${dir}/outside`;
const vectors =
  (args.vectors ? args.vectors.split(",") : [...VECTORS]) as Vector[];
const isMac = Deno.build.os === "darwin";
const cc = Deno.env.get("CC") ?? "cc";

function sh(cmd: string, argv: string[]) {
  const o = new Deno.Command(cmd, {
    args: argv,
    stdout: "piped",
    stderr: "piped",
  }).outputSync();
  if (!o.success) {
    throw new Error(
      `${cmd} ${argv.join(" ")} failed:\n${new TextDecoder().decode(o.stderr)}`,
    );
  }
}

// 1-2. Build and lay out.
Deno.mkdirSync(`${work}/node_modules/corpus-native`, { recursive: true });
Deno.mkdirSync(out, { recursive: true });
const src = `${here}probe.c`;
const shared = isMac ? ["-dynamiclib"] : ["-shared", "-fPIC"];
const addon = isMac
  ? ["-bundle", "-undefined", "dynamic_lookup"]
  : ["-shared", "-fPIC"];
try {
  sh(cc, ["-O1", "-w", "-DPROBE_MAIN", "-o", `${work}/probe`, src]);
  sh(cc, [
    "-O1",
    "-w",
    ...shared,
    "-o",
    `${work}/libprobe.${isMac ? "dylib" : "so"}`,
    src,
  ]);
  sh(cc, [
    "-O1",
    "-w",
    "-DPROBE_NAPI",
    ...addon,
    "-o",
    `${work}/probe.node`,
    src,
  ]);
} catch (e) {
  console.error(
    `corpus: cannot build the probes with ${cc}: ${(e as Error).message}`,
  );
  Deno.exit(2);
}
Deno.copyFileSync(
  `${work}/probe.node`,
  `${work}/node_modules/corpus-native/probe.node`,
);
Deno.writeTextFileSync(
  `${work}/node_modules/corpus-native/package.json`,
  JSON.stringify({ name: "corpus-native", version: "1.0.0", main: "index.js" }),
);
Deno.writeTextFileSync(
  `${work}/node_modules/corpus-native/index.js`,
  'module.exports = require("./probe.node");\n',
);
Deno.writeTextFileSync(
  `${work}/package.json`,
  JSON.stringify({
    name: "corpus-host",
    private: true,
    dependencies: { "corpus-native": "1.0.0" },
  }),
);
Deno.copyFileSync(`${here}host.ts`, `${work}/host.ts`);
Deno.writeTextFileSync(`${work}/benign`, "benign\n");
Deno.writeTextFileSync(`${out}/secret`, "TOP-SECRET outside the grant\n");
Deno.writeTextFileSync(`${out}/secret2`, "TOP-SECRET inherited fd\n");
for (const v of vectors) {
  for (const f of ["victim", "unlink", "ren", "mv"]) {
    Deno.writeTextFileSync(`${out}/${f}-${v}`, "outside\n");
  }
}

// 3. Listeners, outside the confined tree.
const server = new Deno.Command(`${work}/probe`, {
  args: ["serve", out],
  stdin: "piped",
  stdout: "piped",
}).spawn();
const reader = server.stdout.getReader();
let first = "";
while (!first.includes("\n")) {
  const { value, done } = await reader.read();
  if (done) break;
  first += new TextDecoder().decode(value);
}
reader.releaseLock();
const listeners = first.trim();
const grantedPort = /tcpg=(\d+)/.exec(listeners)?.[1];
const cfg = `out=${out};work=${work};${listeners}`;
Deno.writeTextFileSync(
  `${work}/cfg.json`,
  JSON.stringify({ cfg, probes: PROBES.map((p) => p.id) }),
);

// 4. The confined host, once per vector.
const grant = [
  "run",
  "--quiet",
  "--no-prompt",
  `--allow-read=${work}`,
  `--allow-write=${work}`,
  `--allow-ffi=${work}`,
  `--allow-run=${work}/probe`,
  `--allow-net=example.com:${grantedPort}`,
  "--node-modules-dir=manual",
];
type Row = {
  vector: string;
  id: string;
  state: string;
  detail: string;
  cell: string;
};
const rows: Row[] = [];
for (const vector of vectors) {
  const o = new Deno.Command("/bin/sh", {
    // fd 3: opened by the launcher before the run, inherited by the host.
    args: [
      "-c",
      'exec "$0" "$@" 3<"$CORPUS_FD3"',
      Deno.execPath(),
      ...grant,
      `${work}/host.ts`,
      vector,
    ],
    cwd: work,
    env: {
      CORPUS_FD3: `${out}/secret2`,
      CORPUS_SECRET_ENV: "TOP-SECRET env",
      NO_COLOR: "1",
    },
    stdout: "piped",
    stderr: "piped",
  }).outputSync();
  const seen = new Set<string>();
  for (const line of new TextDecoder().decode(o.stdout).split("\n")) {
    if (!line.startsWith("{")) continue;
    const r = JSON.parse(line) as { vector: string; id: string; out: string };
    const [state, ...rest] = r.out.split(" ");
    const cell = PROBES.find((p) => p.id === r.id)!.cell;
    rows.push({ vector, id: r.id, state, detail: rest.join(" "), cell });
    seen.add(r.id);
  }
  for (const p of PROBES) {
    if (!seen.has(p.id)) {
      const err =
        new TextDecoder().decode(o.stderr).trim().split("\n").slice(-1)[0] ??
          "";
      rows.push({
        vector,
        id: p.id,
        state: "crash",
        detail: `host exited ${o.code} before this probe: ${err}`,
        cell: p.cell,
      });
    }
  }
}
server.stdin.close();
await server.status;

// 5. Report.
const allowedToday = new Set(["open", "n/a", "host"]);
const counts: Record<string, number> = {};
for (const r of rows) counts[r.state] = (counts[r.state] ?? 0) + 1;
for (const r of rows) {
  console.log(
    `${r.state.padEnd(5)} ${r.vector.padEnd(5)} ${
      r.id.padEnd(26)
    } [${r.cell}] ${r.detail}`,
  );
}
const bad = expect === "today"
  ? rows.filter((r) => !allowedToday.has(r.state))
  : [];
const summary = Object.entries(counts).sort().map(([k, v]) => `${v} ${k}`).join(
  ", ",
);
console.log(
  `kernel-sandbox corpus: ${PROBES.length} probes x ${vectors.length} vectors = ${rows.length} checks over ${
    new Set(PROBES.map((p) => p.cell)).size
  } of ${
    Object.keys(CELLS).length
  } matrix cells (${Deno.build.os}/${Deno.build.arch}): ${summary}`,
);
if (args.json) {
  Deno.writeTextFileSync(
    args.json,
    JSON.stringify({ os: Deno.build.os, arch: Deno.build.arch, rows }, null, 1),
  );
}
if (!args.dir) Deno.removeSync(dir, { recursive: true });
if (bad.length) {
  console.log(
    `corpus: FAIL (--expect=${expect}): ${bad.length} checks not open: ${
      bad.map((r) => `${r.vector}/${r.id}`).join(" ")
    }`,
  );
  Deno.exit(1);
}
console.log(
  `corpus: ok (--expect=${expect}: nothing held, as expected without kernel enforcement)`,
);
