// Copyright 2018-2026 the Deno authors. MIT license.
// The app origin + memory transport: a Deno.serve app and a node:http app;
// each makes its own checks (see apps/origin/main.ts, apps/originnode/main.ts).

import {
  type AreaReport,
  clearResults,
  type Env,
  exists,
  kill,
  launch,
  launchAndCollect,
  packageApp,
  path,
  rm,
  seenPids,
  tail,
  waitExit,
  writeParams,
} from "../lib/runner.ts";

export async function run(env: Env, rep: AreaReport) {
  const scheme = `dnxorigin${env.nonce}`;
  const origin = `${scheme}://app`;
  const identifier = `dev.denext.e2e${env.nonce}.origin`;
  const p = await packageApp(env, {
    app: "origin",
    name: "E2EOrigin",
    identifier,
    appJson: { origin },
    launch: { appId: identifier, customSchemes: [scheme] },
    include: ["fork_child.js"],
  });
  await clearResults("origin");
  await writeParams("origin", { origin });
  await launchAndCollect(env, rep, "launch", p);

  // The app's executable started as a worker from outside (`<App> run
  // x.js`): it must exit without running the script and without starting
  // the app, whatever the environment claims.
  const dir = path(env.workDir, "worker-argv");
  await rm(dir);
  await Deno.mkdir(dir, { recursive: true });
  const marker = path(dir, "ran");
  const script = path(dir, "x.js");
  await Deno.writeTextFile(
    script,
    `Deno.writeTextFileSync(${JSON.stringify(marker)}, "ran");\n`,
  );
  const forged: [string, Record<string, string>][] = [
    ["argv alone", {}],
    ["argv + NODE_CHANNEL_FD + NEXT_PRIVATE_WORKER", {
      NODE_CHANNEL_FD: "0",
      NEXT_PRIVATE_WORKER: "1",
    }],
    // The runner IS the parent here, but runs another executable.
    ["argv + a token naming the real parent + NODE_CHANNEL_FD", {
      DENO_DESKTOP_WORKER_TOKEN: `v1.${Deno.pid}.${"0".repeat(32)}`,
      NODE_CHANNEL_FD: "0",
    }],
  ];
  for (const [label, envs] of forged) {
    const before = await seenPids("origin");
    const l = await launch(env, p.exe, ["run", script], { env: envs });
    const st = await waitExit(l, 30_000);
    if (!st) await kill(l, p.artifact);
    const ran = await exists(marker);
    const started = [...await seenPids("origin")].some((pid) =>
      !before.has(pid)
    );
    rep.check(
      `worker argv (${label}): exits without running the script or the app`,
      st !== null && st.code !== 0 && !ran && !started,
      {
        code: st?.code ?? "(still running)",
        ran,
        started,
        log: await tail(l.logFile, 6),
      },
    );
    await rm(marker);
  }

  // The same transport with a node:http server as the app's server.
  const nodeScheme = `dnxoriginnode${env.nonce}`;
  const nodeOrigin = `${nodeScheme}://app`;
  const nodeId = `dev.denext.e2e${env.nonce}.originnode`;
  const pn = await packageApp(env, {
    app: "originnode",
    name: "E2EOriginNode",
    identifier: nodeId,
    appJson: { origin: nodeOrigin },
    launch: { appId: nodeId, customSchemes: [nodeScheme] },
  });
  await writeParams("origin", { origin: nodeOrigin });
  await launchAndCollect(env, rep, "node:http", pn);
}
