// Copyright 2018-2026 the Deno authors. MIT license.
// A program the app carries runs only as it was packaged (DENEXT.md item 215):
// packaged with a run grant that records its SHA-256, swapped by this runner
// after the first launch, refused at the next spawn, then written again.

import { sha256 } from "../lib/app-update.ts";
import {
  type AreaReport,
  clearResults,
  type Env,
  launchAndCollect,
  packageApp,
  path,
  writeParams,
} from "../lib/runner.ts";

/** The app's cache folder (the runtime's `desktop_app_cache_dir`). */
function cacheDir(id: string): string {
  const home = Deno.env.get("HOME") ?? Deno.env.get("USERPROFILE") ?? "";
  if (Deno.build.os === "darwin") {
    return path(home, "Library", "Caches", id, "denext");
  }
  if (Deno.build.os === "windows") {
    return path(
      Deno.env.get("LOCALAPPDATA") ?? path(home, "AppData", "Local"),
      id,
      "Cache",
    );
  }
  return path(
    Deno.env.get("XDG_CACHE_HOME") ?? path(home, ".cache"),
    id,
    "denext",
  );
}

export async function run(env: Env, rep: AreaReport) {
  // The run grant's `$DENO_DESKTOP_CACHE` expands from the identifier the
  // fork's CLI writes into the binary's metadata, so the fork's CLI packages
  // this app (FORK_CLI_AREAS). The stock CLI writes none: the runtime would
  // drop the grant (as it should: never widened) and nothing here could pass.
  if (!env.forkDeno) {
    rep.na(
      "a program the app carries runs only as packaged",
      "packaged with the stock CLI, which writes no app identifier for $DENO_DESKTOP_CACHE to expand from; the fork CLI's e2e run covers it",
    );
    return;
  }
  const windows = Deno.build.os === "windows";
  const file = windows ? "prog.exe" : "prog";
  const bytes = windows
    ? await Deno.readFile("C:\\Windows\\System32\\whoami.exe")
    : new TextEncoder().encode("#!/bin/sh\necho packaged\n");
  const hash = await sha256(bytes);
  const identifier = `dev.denext.e2e${env.nonce}.sidecarint`;
  const p = await packageApp(env, {
    app: "sidecarint",
    forkCli: true,
    name: "E2ESidecarInt",
    identifier,
    include: [`bin/${file}`],
    files: { [`bin/${file}`]: bytes },
    permissions: [
      "--allow-read",
      "--allow-env",
      "--allow-net",
      "--allow-sys",
      "--allow-write",
      `--allow-run=$DENO_DESKTOP_CACHE/sidecars/x/${hash}/${file}#sha256=${hash}#embedded=bin/${file}`,
    ],
  });
  const program = path(cacheDir(identifier), "sidecars", "x", hash, file);
  try {
    await clearResults("sidecarint");
    await writeParams("sidecarint", { phase: "first", program });
    await launchAndCollect(env, rep, "first launch", p);
    rep.check(
      "the file holds the packaged bytes",
      await sha256(await Deno.readFile(program)) === hash,
      program,
    );

    // What the app could not do, another program of the user can.
    const swap = `${program}.swap`;
    const evil = windows
      ? await Deno.readFile("C:\\Windows\\System32\\hostname.exe")
      : new TextEncoder().encode("#!/bin/sh\necho swapped\n");
    await Deno.writeFile(swap, evil, { mode: 0o700 });
    await Deno.rename(swap, program);
    await writeParams("sidecarint", { phase: "swapped", program });
    await launchAndCollect(env, rep, "after a swap", p);
    rep.check(
      "the swapped bytes are gone and the packaged ones back",
      await sha256(await Deno.readFile(program)) === hash,
      program,
    );
  } finally {
    await Deno.remove(cacheDir(identifier), { recursive: true }).catch(
      () => {},
    );
  }
}
