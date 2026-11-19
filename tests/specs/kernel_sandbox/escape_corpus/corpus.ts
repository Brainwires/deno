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

import { classify, parse, type Result } from "./classify.ts";
import { type Column, columnFor, expectEnforce } from "./expect.ts";
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
  (args.vectors
    ? args.vectors.split(",")
    : expect === "enforce"
    ? [...VECTORS, "nested", "nested-env", "nested-closed"]
    : [...VECTORS]) as Vector[];
const isMac = Deno.build.os === "darwin";
const cc = Deno.env.get("CC") ?? "cc";
if (!["today", "enforce"].includes(expect)) {
  console.error(`corpus: unknown --expect=${expect} (today, enforce)`);
  Deno.exit(2);
}
// --expect=enforce: the platform column this machine is (macOS, or Linux by
// its Landlock ABI); without a kernel enforcer here there is nothing to hold
let column: Column | undefined;
// whether this runtime's supervising broker (Mode B) holds the network:
// only then can a host-scoped grant be held, and the net-host
// configurations run
let brokerBuilt = false;
if (expect === "enforce") {
  const info = JSON.parse(
    new TextDecoder().decode(
      new Deno.Command(Deno.execPath(), { args: ["info", "--json"] })
        .outputSync().stdout,
    ),
  ).kernelSandbox;
  const why = !info?.enforcerBuilt
    ? "no kernel enforcer for this OS"
    : Deno.build.os === "linux" && info.landlockAbi == null
    ? "no Landlock on this kernel"
    : undefined;
  if (why) {
    console.log(`kernel-sandbox corpus: skipped: ${why}`);
    console.log(`corpus: ok (--expect=enforce skipped: ${why})`);
    Deno.exit(0);
  }
  column = columnFor(Deno.build.os, info.landlockAbi);
  brokerBuilt = info.brokerNetBuilt === true;
}

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
for (const v of [...vectors, "baseline"]) {
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

// 3b. The baseline: every probe once, UNCONFINED (the bare executable, started
// here), under whatever the host itself enforces (a container's seccomp
// profile, a prohibit route, an outer sandbox, Yama). A confined refusal
// counts as "held" only when the baseline was not refused (classify.ts).
const baseline = new Map<string, Result>();
for (const p of PROBES) {
  const o = new Deno.Command(`${work}/probe`, {
    args: [p.id, `${cfg};vec=baseline`],
    env: { CORPUS_SECRET_ENV: "TOP-SECRET env" }, // as the confined runs get it
    stdout: "piped",
    stderr: "null",
  }).outputSync();
  const line = new TextDecoder().decode(o.stdout).trim();
  if (line) baseline.set(p.id, parse(line));
}
const hostRefused = [...baseline].filter(([, r]) =>
  r.state === "held" || r.state === "host"
).map(([id]) => id);
let seccomp = "";
try {
  const st = Deno.readTextFileSync("/proc/self/status");
  seccomp = ["Seccomp", "Seccomp_filters"].map((k) =>
    `${k}=${new RegExp(`^${k}:\\s*(\\d+)`, "m").exec(st)?.[1] ?? "?"}`
  ).join(" ");
} catch {
  // not Linux
}
console.log(
  `baseline (no Deno sandbox${seccomp ? `; ${seccomp}` : ""}): host refuses ${
    hostRefused.join(" ") || "nothing"
  }`,
);

// 4. The confined host, once per vector and configuration.
//    --expect=today: the grant above. --expect=enforce: the configurations
//    the kernel sandbox can hold under --kernel-sandbox=enforce (a name in
//    --allow-net needs the broker): files (read/write/ffi=<work>, no network,
//    no run), the whole network, and a run list (the child vector).
const files = [
  `--allow-read=${work}`,
  `--allow-write=${work}`,
  `--allow-ffi=${work}`,
];
const configs: Record<string, string[]> = expect === "enforce"
  ? {
    files: [...files],
    "net-all": [...files, "--allow-net"],
    run: [...files, `--allow-run=${work}/probe`],
    // the run list names the runtime too: the nested vector's deno
    // (and the launchers the nested-env / nested-closed vectors start it
    // through)
    nested: [
      ...files,
      `--allow-run=${work}/probe,${Deno.execPath()},/usr/bin/env,/bin/bash`,
    ],
    // a host-scoped network grant (the broker holds it): a public name on
    // the granted port, and localhost there, which the host connects to
    // through the broker so the probes hold a socket it handed over
    ...(brokerBuilt
      ? {
        "net-host": [
          ...files,
          `--allow-net=example.com:${grantedPort},localhost:${grantedPort}`,
        ],
        // the same with a run list: the child vector, whose only network
        // is the broker's proxy
        "net-host-run": [
          ...files,
          `--allow-net=example.com:${grantedPort},localhost:${grantedPort}`,
          `--allow-run=${work}/probe`,
        ],
      }
      : {}),
  }
  : {
    today: [
      ...files,
      `--allow-run=${work}/probe`,
      `--allow-net=example.com:${grantedPort}`,
    ],
  };
type Row = {
  config: string;
  vector: string;
  id: string;
  state: string;
  detail: string;
  cell: string;
};
const rows: Row[] = [];
if (expect === "enforce" && !brokerBuilt) {
  console.log(
    "net-host, net-host-run (a host-scoped network grant): skipped, the supervising broker isn't built into this runtime",
  );
}
for (const [config, flags] of Object.entries(configs)) {
  const grant = [
    "run",
    "--quiet",
    "--no-prompt",
    ...(expect === "enforce" ? ["--kernel-sandbox=enforce"] : []),
    ...flags,
    "--node-modules-dir=manual",
  ];
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
      const { state, detail } = classify(parse(r.out), baseline.get(r.id));
      const cell = PROBES.find((p) => p.id === r.id)!.cell;
      rows.push({ config, vector, id: r.id, state, detail, cell });
      seen.add(r.id);
    }
    for (const p of PROBES) {
      if (!seen.has(p.id)) {
        const err =
          new TextDecoder().decode(o.stderr).trim().split("\n").slice(-1)[0] ??
            "";
        rows.push({
          config,
          vector,
          id: p.id,
          state: "crash",
          detail: `host exited ${o.code} before this probe: ${err}`,
          cell: p.cell,
        });
      }
    }
  }
}
server.stdin.close();
await server.status;

// 5. Report.
const allowedToday = new Set(["open", "n/a", "host"]);
const counts: Record<string, number> = {};
for (const r of rows) counts[r.state] = (counts[r.state] ?? 0) + 1;
const expected = (r: Row): Set<string> =>
  expect === "enforce"
    ? expectEnforce(r.cell, column!, r.config, r.vector)
    : allowedToday;
for (const r of rows) {
  const ok = expected(r).has(r.state);
  console.log(
    `${r.state.padEnd(5)} ${r.config.padEnd(7)} ${r.vector.padEnd(5)} ${
      r.id.padEnd(26)
    } [${r.cell}]${
      ok ? "" : ` UNEXPECTED (wants ${[...expected(r)].join("|")})`
    } ${r.detail}`,
  );
}
const bad = rows.filter((r) => !expected(r).has(r.state));
// the nested vectors must have run where their deno may start (all n/a
// there would hide a nested deno that failed to start)
for (const vector of ["nested", "nested-env"]) {
  const nestedRows = rows.filter((r) =>
    r.config === "nested" && r.vector === vector
  );
  if (nestedRows.length && nestedRows.every((r) => r.state === "n/a")) {
    bad.push(...nestedRows.slice(0, 1).map((r) => ({ ...r, state: "crash" })));
    console.log(
      `corpus: the ${vector} deno never ran: ${nestedRows[0].detail}`,
    );
  }
}
// the descriptor closed on the way: the nested deno must have run and
// failed closed (Deno's own check refuses the FFI vector, saying why), not
// failed to start
const closedRows = rows.filter((r) =>
  r.config === "nested" && r.vector === "nested-closed"
);
const notClosed = closedRows.filter((r) => !/grants unknown/.test(r.detail));
if (notClosed.length) {
  bad.push(...notClosed.slice(0, 1).map((r) => ({ ...r, state: "crash" })));
  console.log(
    `corpus: the nested deno with no descriptor didn't fail closed: ${
      notClosed[0].detail
    }`,
  );
}
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
    `corpus: FAIL (--expect=${expect}${
      column ? `, ${column}` : ""
    }): ${bad.length} checks not as expected: ${
      bad.map((r) => `${r.config}/${r.vector}/${r.id}=${r.state}`).join(" ")
    }`,
  );
  Deno.exit(1);
}
console.log(
  expect === "enforce"
    ? `corpus: ok (--expect=enforce, ${column}: every kernel cell held, every other cell as its column documents)`
    : `corpus: ok (--expect=${expect}: nothing held, as expected without kernel enforcement)`,
);
