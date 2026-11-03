// Copyright 2018-2026 the Deno authors. MIT license.
// lib/tree-manifest.ts: what the update area writes into a Windows app before
// it publishes it (the runtime's reader is treecheck.rs).

import {
  readTreeManifest,
  stampTreeManifest,
  treeManifest,
} from "./tree-manifest.ts";

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

/** A minimal PE32+ image: headers, one `.text` section, no resources. */
function minimalPe(): Uint8Array<ArrayBuffer> {
  const b = new Uint8Array(0x400);
  const v = new DataView(b.buffer);
  b.set([0x4d, 0x5a]);
  v.setUint32(0x3c, 0x40, true);
  b.set([0x50, 0x45, 0, 0], 0x40);
  const coff = 0x44;
  v.setUint16(coff, 0x8664, true);
  v.setUint16(coff + 2, 1, true);
  v.setUint16(coff + 16, 240, true);
  v.setUint16(coff + 18, 0x22, true);
  const opt = coff + 20;
  v.setUint16(opt, 0x20b, true);
  v.setUint32(opt + 4, 0x200, true);
  v.setUint32(opt + 16, 0x1000, true);
  v.setUint32(opt + 20, 0x1000, true);
  v.setBigUint64(opt + 24, 0x140000000n, true);
  v.setUint32(opt + 32, 0x1000, true);
  v.setUint32(opt + 36, 0x200, true);
  v.setUint16(opt + 40, 6, true);
  v.setUint16(opt + 48, 6, true);
  v.setUint32(opt + 56, 0x2000, true);
  v.setUint32(opt + 60, 0x200, true);
  v.setUint16(opt + 68, 2, true);
  v.setUint32(opt + 108, 16, true);
  const sec = opt + 240;
  b.set(new TextEncoder().encode(".text"), sec);
  v.setUint32(sec + 8, 0x200, true);
  v.setUint32(sec + 12, 0x1000, true);
  v.setUint32(sec + 16, 0x200, true);
  v.setUint32(sec + 20, 0x200, true);
  v.setUint32(sec + 36, 0x60000020, true);
  b[0x200] = 0xc3;
  return b;
}

Deno.test("the manifest lists every file but the executable, and round-trips", async () => {
  const dir = await Deno.makeTempDir();
  try {
    await Deno.writeFile(`${dir}/App.exe`, minimalPe());
    await Deno.writeFile(`${dir}/runtime.dll`, minimalPe());
    await Deno.writeTextFile(`${dir}/laufey-launch.json`, "{}");
    await Deno.mkdir(`${dir}/res`);
    await Deno.writeTextFile(`${dir}/res/a.pak`, "pak");
    assert(
      readTreeManifest(await Deno.readFile(`${dir}/App.exe`)) === null,
      "a fresh executable carries no manifest",
    );
    const m = await stampTreeManifest(dir, "App.exe", "dev.x.app", "2.0.0");
    assert(
      JSON.stringify(m.files.map((f) => f.path)) ===
        JSON.stringify(["laufey-launch.json", "res/a.pak", "runtime.dll"]),
      `listed: ${m.files.map((f) => f.path)}`,
    );
    const back = readTreeManifest(await Deno.readFile(`${dir}/App.exe`));
    assert(
      JSON.stringify(back) === JSON.stringify(m),
      `read back: ${JSON.stringify(back)}`,
    );
    // Stamping again replaces it: one manifest, for the new identity.
    await stampTreeManifest(dir, "App.exe", "dev.x.other", "2.0.0");
    const again = readTreeManifest(await Deno.readFile(`${dir}/App.exe`));
    assert(again?.appId === "dev.x.other", `restamped: ${again?.appId}`);
    // The executable's own bytes changed, not the listed files.
    const now = await treeManifest(dir, "App.exe", "dev.x.app", "2.0.0");
    assert(
      JSON.stringify(now) === JSON.stringify(m),
      "the listed files are unchanged",
    );
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
