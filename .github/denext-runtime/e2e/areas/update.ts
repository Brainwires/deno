// Copyright 2018-2026 the Deno authors. MIT license.
// Full-app self-update (Deno.desktop.updater), end to end with throwaway
// keys and a throwaway TLS CA:
//
// 1. package the app at 1.0.0 / 2.0.0 / 3.0.0 and "install" 1.0.0;
// 2. hostile manifests and archives against the real app: wrong key,
//    unsigned, wrong app, downgrade, same version, no platform, http, a
//    tampered / oversized download, tar-slip, a symlink out, the wrong shape,
//    and staging without the unsigned-dev opt-out on an unsigned app; none
//    may change the install;
// 3. an install the user can't write (POSIX);
// 4. 1.0.0 -> 2.0.0: download, stage, swap, relaunch, confirm (the old app
//    removed);
// 5. 2.0.0 -> 3.0.0 whose trial exits before confirming: the next launch
//    rolls back to 2.0.0, and 3.0.0 is refused afterwards.
//
// The manifests are served over HTTPS by this process, with a CA the app
// trusts only through `caCerts`.

// deno-lint-ignore-file no-explicit-any

import {
  gzip,
  keyPair,
  packArtifact,
  type Payload,
  sha256,
  sign,
  tar,
} from "../lib/app-update.ts";
import {
  type AreaReport,
  DIR,
  type Env,
  exists,
  kill,
  launch,
  type Launched,
  must,
  OS,
  packageApp,
  path,
  rm,
  sleep,
  tail,
  waitExit,
} from "../lib/runner.ts";

const RES = path(DIR, "update");

async function readJson(p: string): Promise<any> {
  return JSON.parse(await Deno.readTextFile(p));
}

async function resultFile(prefix: string): Promise<string | undefined> {
  for await (const e of Deno.readDir(RES)) {
    if (e.name.startsWith(prefix)) return path(RES, e.name);
  }
  return undefined;
}

async function waitFor(
  pred: () => Promise<boolean>,
  seconds: number,
): Promise<boolean> {
  for (let i = 0; i < seconds * 4; i++) {
    if (await pred()) return true;
    await sleep(250);
  }
  return false;
}

async function copyTree(from: string, to: string) {
  const info = await Deno.lstat(from);
  if (info.isSymlink) return await Deno.symlink(await Deno.readLink(from), to);
  if (info.isFile) {
    await Deno.copyFile(from, to);
    if (info.mode !== null) await Deno.chmod(to, info.mode & 0o777);
    return;
  }
  await Deno.mkdir(to);
  for await (const e of Deno.readDir(from)) {
    await copyTree(path(from, e.name), path(to, e.name));
  }
}

/** A throwaway CA and a leaf for 127.0.0.1. */
async function makeCerts(
  dir: string,
): Promise<{ ca: string; cert: string; key: string }> {
  await Deno.mkdir(dir, { recursive: true });
  await Deno.writeTextFile(
    path(dir, "ca.cnf"),
    "[req]\ndistinguished_name = dn\nx509_extensions = v3_ca\nprompt = no\n[dn]\nCN = denext e2e throwaway CA\n" +
      "[v3_ca]\nbasicConstraints = critical,CA:TRUE\nkeyUsage = critical,keyCertSign,cRLSign\nsubjectKeyIdentifier = hash\n",
  );
  await Deno.writeTextFile(
    path(dir, "leaf.cnf"),
    "[req]\ndistinguished_name = dn\nprompt = no\n[dn]\nCN = 127.0.0.1\n",
  );
  await Deno.writeTextFile(
    path(dir, "ext.cnf"),
    "subjectAltName = IP:127.0.0.1\nbasicConstraints = CA:FALSE\nkeyUsage = digitalSignature,keyEncipherment\nextendedKeyUsage = serverAuth\n",
  );
  const o = { cwd: dir };
  await must("openssl", [
    "req",
    "-x509",
    "-newkey",
    "rsa:2048",
    "-nodes",
    "-keyout",
    "ca.key",
    "-out",
    "ca.pem",
    "-days",
    "2",
    "-config",
    "ca.cnf",
  ], o);
  await must("openssl", [
    "req",
    "-new",
    "-newkey",
    "rsa:2048",
    "-nodes",
    "-keyout",
    "leaf.key",
    "-out",
    "leaf.csr",
    "-config",
    "leaf.cnf",
  ], o);
  await must("openssl", [
    "x509",
    "-req",
    "-in",
    "leaf.csr",
    "-CA",
    "ca.pem",
    "-CAkey",
    "ca.key",
    "-CAcreateserial",
    "-out",
    "leaf.pem",
    "-days",
    "2",
    "-extfile",
    "ext.cnf",
  ], o);
  return {
    ca: path(dir, "ca.pem"),
    cert: path(dir, "leaf.pem"),
    key: path(dir, "leaf.key"),
  };
}

export async function run(env: Env, rep: AreaReport) {
  const work = path(env.workDir, "update");
  await rm(work);
  await rm(RES);
  await Deno.mkdir(work, { recursive: true });
  await Deno.mkdir(RES, { recursive: true });
  const ID = `dev.denext.e2e${env.nonce}.update`;
  const NAME = "E2EUpdate";
  const { key, publicKey } = await keyPair();
  const { key: otherKey } = await keyPair();
  const certs = await makeCerts(path(work, "certs"));

  // 1. Package and install.
  const artifacts: Record<string, string> = {};
  const builds: string[] = [];
  for (const v of ["1.0.0", "2.0.0", "3.0.0"]) {
    const p = await packageApp(env, {
      app: "update",
      tag: v,
      name: NAME,
      identifier: ID,
      version: v,
      appJson: { update: { publicKey } },
      launch: { appId: ID },
    });
    artifacts[v] = p.artifact;
    builds.push(p.buildDir);
  }
  const top = artifacts["1.0.0"].split(/[\\/]/).pop()!;
  const installParent = path(work, "install");
  await Deno.mkdir(installParent);
  const install = path(installParent, top);
  await copyTree(artifacts["1.0.0"], install);
  const exe = OS === "darwin"
    ? path(
      install,
      "Contents",
      "MacOS",
      (await Array.fromAsync(Deno.readDir(path(install, "Contents", "MacOS"))))
        .find((e) => !e.name.endsWith(".dylib"))!.name,
    )
    : path(install, OS === "windows" ? `${NAME}.exe` : NAME);
  rep.check("1.0.0 installed", await exists(exe), exe);

  // 2. The HTTPS server with 2.0.0, 3.0.0 and the hostile variants.
  const srv = path(work, "srv");
  await Deno.mkdir(path(srv, "adv"), { recursive: true });
  const platform = `${env.target}-${env.backend}`;
  let base = "";
  const server = Deno.serve({
    hostname: "127.0.0.1",
    port: 0,
    cert: await Deno.readTextFile(certs.cert),
    key: await Deno.readTextFile(certs.key),
    onListen() {},
  }, async (req) => {
    const p = decodeURIComponent(new URL(req.url).pathname);
    const file = path(srv, ...p.split("/").filter((s) => s && s !== ".."));
    try {
      const bytes = await Deno.readFile(file);
      if (p.endsWith("oversize.tar.gz")) {
        // Chunked (no Content-Length), 64 KiB more than declared.
        return new Response(
          new ReadableStream<Uint8Array>({
            start(c) {
              for (let i = 0; i < bytes.length; i += 65536) {
                c.enqueue(bytes.slice(i, i + 65536));
              }
              c.enqueue(new Uint8Array(65536));
              c.close();
            },
          }),
        );
      }
      return new Response(bytes);
    } catch {
      return new Response("not found", { status: 404 });
    }
  });
  base = `https://127.0.0.1:${server.addr.port}`;
  try {
    const payloads: Record<string, Payload> = {};
    for (const v of ["2.0.0", "3.0.0"]) {
      const d = path(srv, `v${v[0]}`);
      await Deno.mkdir(d, { recursive: true });
      const archiveName = `${ID}-${v}-${platform}.tar.gz`;
      const t0 = Date.now();
      const { sha256: sum, size } = await packArtifact(
        artifacts[v],
        top,
        path(d, archiveName),
      );
      payloads[v] = {
        schema: 1,
        app: ID,
        version: v,
        platforms: {
          [platform]: {
            url: `${base}/v${v[0]}/${archiveName}`,
            sha256: sum,
            size,
            kind: "bundle",
          },
        },
        publishedAt: new Date().toISOString(),
      };
      await Deno.writeTextFile(
        path(d, "app-update.json"),
        await sign(payloads[v], key),
      );
      console.log(`  published ${v}: ${size} bytes in ${Date.now() - t0} ms`);
    }
    for (const b of builds.slice(1)) await rm(b);

    const v2 = payloads["2.0.0"];
    const v2entry = v2.platforms[platform];
    const withEntry = (
      e: Partial<typeof v2entry>,
      over: Partial<Payload> = {},
    ) => ({ ...v2, ...over, platforms: { [platform]: { ...v2entry, ...e } } });
    const cases: Record<
      string,
      { url: string; download?: boolean; optOut?: boolean }
    > = {};
    const adv = path(srv, "adv");
    const addCase = async (
      n: string,
      body: string,
      o: { download?: boolean; optOut?: boolean } = {},
    ) => {
      await Deno.writeTextFile(path(adv, `${n}.json`), body);
      cases[n] = { url: `${base}/adv/${n}.json`, ...o };
    };
    const addArchive = async (n: string, bytes: Uint8Array) => {
      await Deno.writeFile(path(adv, `${n}.tar.gz`), bytes);
      return {
        url: `${base}/adv/${n}.tar.gz`,
        sha256: await sha256(bytes),
        size: bytes.length,
      };
    };
    await addCase("wrongkey", await sign(v2, otherKey));
    await addCase("unsigned", JSON.stringify(v2));
    await addCase("wrongapp", await sign({ ...v2, app: `${ID}.other` }, key));
    await addCase("downgrade", await sign({ ...v2, version: "0.9.0" }, key));
    await addCase("equal", await sign({ ...v2, version: "1.0.0" }, key));
    await addCase(
      "noplatform",
      await sign({
        ...v2,
        platforms: { "aarch64-pc-windows-msvc-cef": v2entry },
      }, key),
    );
    await addCase(
      "http",
      await sign(withEntry({ url: "http://example.com/a.tar.gz" }), key),
    );
    const real = await Deno.readFile(
      path(srv, "v2", v2entry.url.split("/").pop()!),
    );
    const tampered = real.slice();
    tampered[Math.floor(tampered.length / 2)] ^= 0xff;
    await Deno.writeFile(path(adv, "tampered.tar.gz"), tampered);
    await addCase(
      "tampered",
      await sign(withEntry({ url: `${base}/adv/tampered.tar.gz` }), key),
      { download: true },
    );
    await Deno.writeFile(path(adv, "oversize.tar.gz"), real);
    await addCase(
      "oversize",
      await sign(withEntry({ url: `${base}/adv/oversize.tar.gz` }), key),
      { download: true },
    );
    const slip = await addArchive(
      "slip",
      await gzip(
        tar([[`${top}/`, "5", new Uint8Array()], [
          `${top}/x`,
          "0",
          new Uint8Array([1]),
        ], ["../evil", "0", new Uint8Array([2])]]),
      ),
    );
    await addCase("tarslip", await sign(withEntry(slip), key), {
      download: true,
    });
    const link = await addArchive(
      "link",
      await gzip(
        tar([[`${top}/`, "5", new Uint8Array()], [
          `${top}/l`,
          "2",
          new Uint8Array(),
          OS === "windows" ? "x" : "../../../etc",
        ]]),
      ),
    );
    await addCase("symlink", await sign(withEntry(link), key), {
      download: true,
    });
    const shape = await addArchive(
      "shape",
      await gzip(
        tar([["Other/", "5", new Uint8Array()], [
          "Other/readme",
          "0",
          new Uint8Array([1]),
        ]]),
      ),
    );
    await addCase("wrongshape", await sign(withEntry(shape), key), {
      download: true,
    });
    await addCase(
      "noOptOut",
      await Deno.readTextFile(path(srv, "v2", "app-update.json")),
      { download: true, optOut: false },
    );

    const running: Launched[] = [];
    const start = async (tag: string) => {
      const l = await launch(env, exe, [], { cwd: work });
      running.push(l);
      void tag;
      return l;
    };
    const writeProbe = (p: Record<string, unknown>) =>
      Deno.writeTextFile(
        path(RES, "probe.json"),
        JSON.stringify({ caCert: certs.ca, ...p }),
      );
    const listParent = async () =>
      (await Array.fromAsync(Deno.readDir(installParent))).map((e) => e.name)
        .sort();

    // 3. Adversarial.
    await writeProbe({ mode: "adversarial", cases });
    let l = await start("adversarial");
    const advOk = await waitFor(
      async () => !!(await resultFile("adversarial-1.0.0")),
      240,
    );
    rep.check(
      "adversarial: the app ran every case",
      advOk,
      advOk ? undefined : await tail(l.logFile),
    );
    await waitExit(l, 30_000);
    if (advOk) {
      const r = await readJson((await resultFile("adversarial-1.0.0"))!);
      const want: Record<string, string> = {
        wrongkey: "signature",
        unsigned: "invalid_manifest",
        wrongapp: "wrong_app",
        downgrade: "downgrade",
        equal: "not available",
        noplatform: "no_platform",
        http: "insecure_url",
        tampered: "integrity",
        oversize: "size_exceeded",
        tarslip: "unsafe_archive",
        symlink: "unsafe_archive",
        wrongshape: "bundle_mismatch",
        noOptOut: OS === "linux" ? "staged" : "os_signature",
      };
      for (const [n, code] of Object.entries(want)) {
        const got = r[n];
        const ok = code === "not available"
          ? got?.ok?.available === false
          : code === "staged"
          ? !!got?.ok
          : got?.code === code;
        rep.check(`adversarial: ${n} -> ${code}`, ok, got);
      }
    }
    const afterAdv = await listParent();
    rep.check(
      "adversarial: the install is untouched (no .old)",
      !afterAdv.some((n) => n.endsWith(".old")),
      afterAdv,
    );

    // 4. Install not writable (POSIX permissions).
    if (OS !== "windows") {
      await Deno.chmod(installParent, 0o555);
      await rm(path(RES, "adversarial-1.0.0.json"));
      await writeProbe({
        mode: "adversarial",
        cases: {
          notWritable: { url: `${base}/v2/app-update.json`, download: true },
        },
      });
      l = await start("not-writable");
      const ok = await waitFor(
        async () => !!(await resultFile("adversarial-1.0.0")),
        180,
      );
      await waitExit(l, 30_000);
      await Deno.chmod(installParent, 0o755);
      const r = ok
        ? await readJson((await resultFile("adversarial-1.0.0"))!)
        : null;
      rep.check(
        "an install the user can't write -> install_not_writable",
        r?.notWritable?.code === "install_not_writable",
        r ?? await tail(l.logFile),
      );
    } else {
      rep.na(
        "an install the user can't write -> install_not_writable",
        "the runner is an administrator, which Windows ACLs on a scratch directory don't stop",
      );
    }

    // 5. 1.0.0 -> 2.0.0, relaunch, confirm.
    await writeProbe({
      mode: "update",
      updateFrom: "1.0.0",
      manifest: `${base}/v2/app-update.json`,
      crashTrialVersion: "3.0.0",
    });
    l = await start("update-1");
    const confirmed2 = await waitFor(
      async () => !!(await resultFile("confirmed-2.0.0")),
      300,
    );
    rep.check(
      "1.0.0 -> 2.0.0: the relaunched 2.0.0 confirmed",
      confirmed2,
      confirmed2 ? undefined : {
        events: await Deno.readTextFile(path(RES, "events.log")).catch(() =>
          ""
        ),
        log: await tail(l.logFile),
        helper: await Deno.readTextFile(
          path(installParent, `.${top}.denext-update.log`),
        ).catch(() => "(no helper log)"),
      },
    );
    const staged = await resultFile("staged-1.0.0").then((p) =>
      p ? readJson(p) : null
    );
    rep.check(
      "1.0.0: download reported progress to the archive size",
      staged?.progress === v2entry.size && staged?.progressEvents > 0 &&
        staged?.dl?.ok?.size === v2entry.size,
      staged,
    );
    if (confirmed2) {
      const c = await readJson((await resultFile("confirmed-2.0.0"))!);
      rep.check("2.0.0: confirm() returned true", c.confirmed === true, c);
      const st = await readJson((await resultFile("status-2.0.0"))!);
      rep.check(
        "2.0.0 launched as the trial, updatedFrom 1.0.0",
        st.updatedFrom === "1.0.0" && st.trial === true,
        st,
      );
    }
    await waitFor(
      async () => !(await listParent()).some((n) => n.endsWith(".old")),
      15,
    );
    const after2 = await listParent();
    rep.check(
      "the previous app is removed after confirm",
      !after2.some((n) => n.endsWith(".old")),
      after2,
    );
    const statePath = path(installParent, `.${top}.denext-update.json`);
    const state2 = await readJson(statePath).catch(() => null);
    rep.check(
      "update state idle after confirm",
      state2?.phase === "idle",
      state2,
    );
    if (OS === "darwin") {
      const v = await must("codesign", [
        "--verify",
        "--deep",
        "--strict",
        install,
      ]).then(() => "ok", (e) => String(e));
      rep.check(
        "the swapped bundle passes codesign --verify --deep --strict",
        v === "ok",
        v,
      );
    }
    for (const x of running.splice(0)) await kill(x);

    // 6. 2.0.0 -> 3.0.0 whose trial exits before confirming: rollback.
    await writeProbe({
      mode: "update",
      updateFrom: "2.0.0",
      manifest: `${base}/v3/app-update.json`,
      crashTrialVersion: "3.0.0",
    });
    l = await start("update-2");
    const crashed = await waitFor(
      async () =>
        (await Deno.readTextFile(path(RES, "events.log")).catch(() => ""))
          .includes("crashing trial 3.0.0"),
      300,
    );
    rep.check("3.0.0's trial launched (and exits before confirming)", crashed);
    await sleep(3000);
    const state3 = await readJson(statePath).catch(() => null);
    rep.check(
      "state: swapped, one unconfirmed launch",
      state3?.phase === "swapped" && state3?.launches === 1,
      state3,
    );
    await rm(path(RES, "check-2.0.0.json"));
    l = await start("after-crash");
    const rolled = await waitFor(async () => {
      for await (const e of Deno.readDir(RES)) {
        if (e.name.startsWith("status-2.0.0-")) {
          const s = await readJson(path(RES, e.name)).catch(() => ({}));
          if (s.rolledBackFrom === "3.0.0") return true;
        }
      }
      return false;
    }, 180);
    rep.check(
      "the next launch rolled back to 2.0.0 (rolledBackFrom 3.0.0)",
      rolled,
      rolled
        ? undefined
        : await Deno.readTextFile(path(RES, "events.log")).catch(() => ""),
    );
    await waitFor(async () => !!(await resultFile("check-2.0.0")), 60);
    const recheck = await resultFile("check-2.0.0").then((p) =>
      p ? readJson(p) : null
    );
    rep.check(
      "3.0.0 is refused after the rollback (rejected)",
      recheck?.code === "rejected",
      recheck,
    );
    const after3 = await listParent();
    rep.check(
      "nothing left of the failed version",
      !after3.some((n) => n.endsWith(".old") || n.includes("denext-failed")),
      after3,
    );
    const state4 = await readJson(statePath).catch(() => null);
    rep.check(
      "state idle with rejected 3.0.0",
      state4?.phase === "idle" && state4?.rejected === "3.0.0",
      state4,
    );
    if (OS === "darwin") {
      const v = await must("codesign", [
        "--verify",
        "--deep",
        "--strict",
        install,
      ]).then(() => "ok", (e) => String(e));
      rep.check(
        "the rolled-back bundle passes codesign --verify --deep --strict",
        v === "ok",
        v,
      );
    }
    for (const x of running.splice(0)) await kill(x);
    rep.na(
      "a staged update signed by the same Developer ID / Authenticode signer is accepted",
      "it needs a real code-signing identity, which the fork's CI has no business holding; the refusal for an unsigned app (os_signature) is checked above",
    );
  } finally {
    await server.shutdown();
    // The helper's log and the update state, for the uploaded results.
    for (
      const f of [`.${top}.denext-update.log`, `.${top}.denext-update.json`]
    ) {
      await Deno.copyFile(path(installParent, f), path(RES, f.slice(1))).catch(
        () => {},
      );
    }
    await rm(work);
  }
}
