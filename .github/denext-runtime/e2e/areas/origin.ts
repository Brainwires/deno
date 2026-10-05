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
    // Single instance on, with the launch file pinning the app id: the
    // app's forked workers (both fork shapes) start while it holds the lock
    // and must run, not be forwarded to it as a second instance.
    appJson: { origin, singleInstance: true },
    // Every origin may reach the runtime's bindings (the backend's own gate
    // defaults to the app's scheme): the app checks the runtime's per-binding
    // gate by navigating to a page at another origin.
    launch: {
      appId: identifier,
      customSchemes: [scheme],
      bridgeOrigins: ["*"],
      singleInstance: true,
    },
    include: ["fork_child.js"],
  });
  await clearResults("origin");
  await writeParams("origin", { origin, backend: env.backend });
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
  const token = `v1.${Deno.pid}.${"0".repeat(32)}`;
  const forged: [string, string[], Record<string, string>][] = [
    ["argv alone", ["run", script], {}],
    ["argv + NODE_CHANNEL_FD + NEXT_PRIVATE_WORKER", ["run", script], {
      NODE_CHANNEL_FD: "0",
      NEXT_PRIVATE_WORKER: "1",
    }],
    // The runner IS the parent here, but runs another executable.
    ["argv + a token naming the real parent + NODE_CHANNEL_FD", [
      "run",
      script,
    ], { DENO_DESKTOP_WORKER_TOKEN: token, NODE_CHANNEL_FD: "0" }],
    // The shape a compiled binary's fork() uses: the module in an env var.
    ["DENO_INTERNAL_CHILD_ENTRYPOINT + NODE_CHANNEL_FD", [script], {
      DENO_INTERNAL_CHILD_ENTRYPOINT: script,
      NODE_CHANNEL_FD: "0",
    }],
    ["DENO_INTERNAL_CHILD_ENTRYPOINT + a token + NODE_CHANNEL_FD", [script], {
      DENO_INTERNAL_CHILD_ENTRYPOINT: script,
      DENO_DESKTOP_WORKER_TOKEN: token,
      NODE_CHANNEL_FD: "0",
    }],
  ];
  for (const [label, args, envs] of forged) {
    const before = await seenPids("origin");
    const l = await launch(env, p.exe, args, { env: envs });
    const st = await waitExit(l, 30_000);
    if (!st) await kill(l, p.artifact);
    const ran = await exists(marker);
    const started = [...await seenPids("origin")].some((pid) =>
      !before.has(pid)
    );
    rep.check(
      `worker launch (${label}): exits without running the script or the app`,
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

  // The development switches in a packaged app's environment: ignored.
  const dsScheme = `dnxdevswitch${env.nonce}`;
  const dsOrigin = `${dsScheme}://app`;
  const dsId = `dev.denext.e2e${env.nonce}.devswitch`;
  const ds = await packageApp(env, {
    app: "devswitch",
    name: "E2EDevSwitch",
    identifier: dsId,
    appJson: { origin: dsOrigin },
    launch: { appId: dsId, customSchemes: [dsScheme] },
  });
  const watched = path(env.workDir, "devswitch-hmr");
  await rm(watched);
  await Deno.mkdir(watched, { recursive: true });
  const probe = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const inspectPort = (probe.addr as Deno.NetAddr).port;
  probe.close();
  await writeParams("origin", { origin: dsOrigin });
  await launchAndCollect(env, rep, "devswitch", ds, {
    env: {
      DENO_DESKTOP_HMR: watched,
      DENO_DESKTOP_DEV_URL: "http://127.0.0.1:9/",
      DENO_DESKTOP_FRAMEWORK_DEV: "1",
      DENO_DESKTOP_INSPECT_INTERNAL_PORT: `127.0.0.1:${inspectPort}`,
    },
  });
  await rm(watched);

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
