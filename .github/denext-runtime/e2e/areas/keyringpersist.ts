// Copyright 2018-2026 the Deno authors. MIT license.
// A profile's cookies encrypted with the OS key survive a launch that can't
// reach the key (laufey API 45; apps/keyringpersist/main.ts).
//
// Chromium drops a cookie it can't decrypt and then deletes that site's
// cookies from its database, so a profile holding OS-key ("v11") cookies
// must never be switched to --password-store=basic, not even for one
// launch: the runtime keeps the OS store and waits for the key instead.
// Needs E2E_SECRET_SERVICE=locked (e2e.sh: a gnome-keyring on the session
// bus, no one to answer its prompt); this area restarts that keyring locked
// or unlocked between launches of one app (one profile):
//
//   1. locked, a fresh profile: basic (nothing to lose); a cookie (v10);
//   2. locked, a profile with only v10 rows: basic again, nothing stalls;
//   3. unlocked: the OS key; the v10 cookie reads, a new one is v11;
//   4. locked: the OS store is kept and waits for the key (the warning on
//      stderr, cookieEncryptionWait), then the launch is ended; the v11 row
//      is still in the database;
//   5. unlocked: both cookies are still there.

import {
  type AreaReport,
  clearResults,
  type Env,
  exists,
  kill,
  launch,
  log,
  packageApp,
  type Packaged,
  path,
  rm,
  seenPids,
  sh,
  short,
  tail,
  waitExit,
  waitResult,
  writeParams,
} from "../lib/runner.ts";

const AREA = "keyringpersist";
const PASSWORD = "e2e-keyring";
const SECRETS = "org.freedesktop.secrets";

// The keyring session.sh started (E2E_SECRET_SERVICE=locked), restarted
// locked or unlocked with the same files.
class Keyring {
  child: Deno.ChildProcess | null = null;
  constructor(readonly dir: string) {}

  async ownerPid(): Promise<number | null> {
    const r = await sh("busctl", [
      "--user",
      "--auto-start=no",
      "call",
      "org.freedesktop.DBus",
      "/org/freedesktop/DBus",
      "org.freedesktop.DBus",
      "GetConnectionUnixProcessID",
      "s",
      SECRETS,
    ]);
    const m = /^u (\d+)/.exec(r.out.trim());
    return r.code === 0 && m ? Number(m[1]) : null;
  }

  async locked(): Promise<string> {
    // Never --auto-start: that would start the machine's own keyring.
    const r = await sh("busctl", [
      "--user",
      "--auto-start=no",
      "get-property",
      SECRETS,
      "/org/freedesktop/secrets/aliases/default",
      "org.freedesktop.Secret.Collection",
      "Locked",
    ]);
    return r.code === 0 ? r.out.trim() : "";
  }

  async stop() {
    const pid = await this.ownerPid();
    if (pid) {
      try {
        Deno.kill(pid, "SIGTERM");
      } catch { /* gone */ }
    }
    for (let i = 0; i < 100 && await this.ownerPid(); i++) {
      await new Promise((r) => setTimeout(r, 100));
    }
    if (this.child) {
      await this.child.status.catch(() => {});
      this.child = null;
    }
  }

  /** Restart it; true when its default collection then reads `unlocked`. */
  async restart(unlocked: boolean): Promise<boolean> {
    await this.stop();
    const args = ["--foreground", "--components=secrets"];
    if (unlocked) args.push("--unlock");
    this.child = new Deno.Command("gnome-keyring-daemon", {
      args,
      env: {
        XDG_DATA_HOME: path(this.dir, "data"),
        XDG_RUNTIME_DIR: path(this.dir, "run"),
      },
      stdin: "piped",
      stdout: "null",
      stderr: "null",
    }).spawn();
    const w = this.child.stdin.getWriter();
    if (unlocked) await w.write(new TextEncoder().encode(PASSWORD));
    await w.close();
    const want = unlocked ? "b false" : "b true";
    let state = "";
    for (let i = 0; i < 100; i++) {
      state = await this.locked();
      if (state === want) return true;
      await new Promise((r) => setTimeout(r, 100));
    }
    log(`  keyring: Locked is "${state}", wanted "${want}"`);
    return false;
  }
}

interface Row {
  name: string;
  prefix: string;
}

/** The profile's cookie rows (name, encrypted_value's first 3 bytes), read
 * with Python's sqlite3 (read-only); null when the database can't be read. */
async function cookieRows(db: string): Promise<Row[] | null> {
  if (!await exists(db)) return [];
  const py = `
import json, sqlite3, sys
c = sqlite3.connect("file:" + sys.argv[1] + "?mode=ro", uri=True)
rows = c.execute("SELECT name, substr(encrypted_value, 1, 3) FROM cookies").fetchall()
print(json.dumps([{"name": n, "prefix": bytes(p).decode("latin-1")} for n, p in rows]))
`;
  const r = await sh("python3", ["-c", py, db]);
  if (r.code !== 0) {
    log(`  cookie rows: ${short(r.out)}`);
    return null;
  }
  return JSON.parse(r.out.trim().split("\n").pop()!);
}

const WARNING = "this profile holds cookies encrypted with the OS key";

export async function run(env: Env, rep: AreaReport) {
  if (!env.target.endsWith("-linux-gnu") || env.backend !== "cef") {
    rep.na(
      "OS-key cookies survive a launch that can't reach the key",
      "Linux CEF only: Chromium's cookie key is in the Secret Service there",
    );
    return;
  }
  const keyringDir = Deno.env.get("E2E_KEYRING_DIR");
  if (Deno.env.get("E2E_SECRET_SERVICE") !== "locked" || !keyringDir) {
    rep.na(
      "OS-key cookies survive a launch that can't reach the key",
      "needs E2E_SECRET_SERVICE=locked (a gnome-keyring this area can lock and unlock)",
    );
    return;
  }
  const identifier = `dev.denext.e2e${env.nonce}.keyringpersist`;
  // A persistent profile (the app id's data directory), kept across the
  // launches.
  const p = await packageApp(env, {
    app: "keyringpersist",
    name: "E2EKeyringPersist",
    identifier,
    launch: { appId: identifier },
  });
  const dataHome = Deno.env.get("XDG_DATA_HOME") ||
    path(Deno.env.get("HOME") ?? ".", ".local", "share");
  const profileRoot = path(dataHome, identifier);
  const db = path(profileRoot, "CEF", "Default", "Cookies");
  const keyring = new Keyring(keyringDir);
  try {
    // 1. Locked, a fresh profile: basic; a cookie written under it (v10).
    rep.check("1: the keyring restarts locked", await keyring.restart(false));
    await step(env, rep, p, "1 locked, fresh profile", {
      step: "1",
      set: "basic_cookie",
      expect: ["basic_cookie"],
      cookieEncryption: "basic",
      wait: false,
    });
    let rows = await cookieRows(db);
    rep.check(
      "1: the cookie is stored under basic (v10), no OS-key (v11) rows",
      !!rows && rows.some((r) =>
        r.name === "basic_cookie" && r.prefix === "v10"
      ) &&
        !rows.some((r) => r.prefix === "v11"),
      rows,
    );

    // 2. Locked, the database holds only v10 rows: basic again, no stall.
    await step(env, rep, p, "2 locked, only v10 cookies", {
      step: "2",
      expect: ["basic_cookie"],
      cookieEncryption: "basic",
      wait: false,
    });

    // 3. Unlocked: the OS key. The v10 cookie still reads; a new one is
    // written under the OS key (v11).
    rep.check("3: the keyring restarts unlocked", await keyring.restart(true));
    await step(env, rep, p, "3 unlocked", {
      step: "3",
      set: "os_cookie",
      expect: ["basic_cookie", "os_cookie"],
      cookieEncryption: "os",
      wait: false,
    });
    rows = await cookieRows(db);
    rep.check(
      "3: the new cookie is stored under the OS key (v11)",
      !!rows && rows.some((r) => r.name === "os_cookie" && r.prefix === "v11"),
      rows,
    );

    // 4. Locked again, the profile holds a v11 row: the OS store is kept
    // (never basic) and the cookie store waits for the key. Ended once the
    // decision is reported; the rows must all still be there.
    rep.check("4: the keyring restarts locked", await keyring.restart(false));
    const log4 = await step(env, rep, p, "4 locked, OS-key cookies", {
      step: "4",
      stall: true,
      cookieEncryption: "os",
      wait: true,
    });
    const stderr = log4 ? await Deno.readTextFile(log4).catch(() => "") : "";
    const warning = stderr.split("\n").find((l) => l.includes(WARNING));
    rep.check(
      "4: one stderr warning says the cookie store waits for the OS key",
      !!warning && warning.includes("waits until it is unlocked") &&
        stderr.split(WARNING).length === 2,
      warning ?? (log4 ? await tail(log4) : "no log"),
    );
    rows = await cookieRows(db);
    rep.check(
      "4: the OS-key cookie is still in the database",
      !!rows && rows.some((r) =>
        r.name === "os_cookie" && r.prefix === "v11"
      ) &&
        rows.some((r) => r.name === "basic_cookie"),
      rows,
    );

    // 5. Unlocked: both cookies are still there.
    rep.check("5: the keyring restarts unlocked", await keyring.restart(true));
    await step(env, rep, p, "5 unlocked again", {
      step: "5",
      expect: ["basic_cookie", "os_cookie"],
      cookieEncryption: "os",
      wait: false,
    });
    rep.note(`cookie rows at the end: ${JSON.stringify(await cookieRows(db))}`);
  } finally {
    // Leave session.sh a locked keyring, as it started one.
    await keyring.restart(false).catch(() => false);
    await rm(profileRoot);
  }
}

/** One launch: its checks merged under `label`; the launch log's path. */
async function step(
  env: Env,
  rep: AreaReport,
  p: Packaged,
  label: string,
  params: Record<string, unknown>,
): Promise<string | null> {
  await clearResults(AREA);
  await writeParams(AREA, params);
  const seen = await seenPids(AREA);
  const l = await launch(env, p.exe);
  const r = await waitResult(AREA, { seen, ms: 120_000 });
  rep.merge(label, r, l.logFile);
  if (!r) {
    rep.check(`${label}: finished within 120 s`, false, await tail(l.logFile));
  } else if (!params.stall) {
    const st = await waitExit(l, 30_000);
    rep.check(
      `${label}: the app exited`,
      st !== null,
      st ?? await tail(l.logFile),
    );
  }
  await kill(l, p.artifact);
  await waitExit(l, 10_000);
  return l.logFile;
}
