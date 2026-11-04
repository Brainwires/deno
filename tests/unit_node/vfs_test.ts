// Copyright 2018-2026 the Deno authors. MIT license.

import { assert, assertEquals, assertRejects, assertThrows } from "@std/assert";
import * as fs from "node:fs";
import * as fsPromises from "node:fs/promises";
import * as path from "node:path";
import { create, MemoryProvider, RealFSProvider } from "node:vfs";

// `deno eval` grants every permission, so the permission cases run a script
// file under `deno run` instead.
function scriptFile(source: string): string {
  const file = path.join(Deno.makeTempDirSync(), "main.mjs");
  Deno.writeTextFileSync(file, source);
  return file;
}

function mountedVfs() {
  const vfs = create(new MemoryProvider());
  vfs.mkdirSync("/dir", { recursive: true });
  vfs.writeFileSync("/dir/a.txt", "hello");
  const mountPoint = path.join(Deno.makeTempDirSync(), "mnt");
  vfs.mount(mountPoint);
  return { vfs, mountPoint };
}

Deno.test("[node/vfs] node:fs reaches a mounted virtual file system", async () => {
  const { vfs, mountPoint } = mountedVfs();
  const file = path.join(mountPoint, "dir", "a.txt");
  try {
    assertEquals(fs.readFileSync(file, "utf8"), "hello");
    assert(fs.existsSync(file));
    assertEquals(fs.readdirSync(path.join(mountPoint, "dir")), ["a.txt"]);
    assertEquals(fs.statSync(file).size, 5);

    fs.writeFileSync(path.join(mountPoint, "dir", "b.txt"), "written");
    // Once mounted, the VFS's own API takes the mounted paths too.
    assertEquals(
      vfs.readFileSync(path.join(mountPoint, "dir", "b.txt"), "utf8"),
      "written",
    );

    const fd = fs.openSync(file, "r");
    const buf = Buffer.alloc(5);
    assertEquals(fs.readSync(fd, buf, 0, 5, 0), 5);
    assertEquals(buf.toString(), "hello");
    fs.closeSync(fd);

    assertEquals(await fsPromises.readFile(file, "utf8"), "hello");
    const data = await new Promise<string>((resolve, reject) =>
      fs.readFile(file, "utf8", (err, d) => err ? reject(err) : resolve(d!))
    );
    assertEquals(data, "hello");
  } finally {
    vfs.unmount();
  }
  // Once unmounted, the path is the real (empty) directory again.
  assertThrows(() => fs.readFileSync(file), Error, "ENOENT");
});

Deno.test("[node/vfs] a missing mounted path is ENOENT without touching the disk", async () => {
  const { vfs, mountPoint } = mountedVfs();
  try {
    const missing = path.join(mountPoint, "nope.txt");
    const err = assertThrows(() =>
      fs.readFileSync(missing)
    ) as NodeJS.ErrnoException;
    assertEquals(err.code, "ENOENT");
    await assertRejects(
      () => fsPromises.stat(missing),
      Error,
      "ENOENT",
    );
  } finally {
    vfs.unmount();
  }
});

// A virtual stat without an explicit owner defaults to the process's uid /
// gid. Reading those needs `sys` permission in Deno: without it the stat must
// neither prompt nor fail, and reports 0.
Deno.test("[node/vfs] virtual stats don't need sys permission", async () => {
  const script = `
    import { createRequire } from "node:module";
    import process from "node:process";
    const require = createRequire(import.meta.url);
    const { createFileStats } = require("internal/vfs/stats");
    const st = createFileStats(1);
    const expected = Deno.args[0] === "sys" ? process.getuid() : 0;
    console.log(st.uid === expected ? "ok" : "uid " + st.uid);
  `;
  const file = scriptFile(script);
  for (const mode of ["none", "sys"]) {
    const args = ["run", "--no-prompt"];
    if (mode === "sys") args.push("--allow-sys");
    args.push(file, mode);
    const out = await new Deno.Command(Deno.execPath(), {
      args,
      stdout: "piped",
      stderr: "piped",
    }).output();
    const stderr = new TextDecoder().decode(out.stderr);
    assert(out.success, stderr);
    assertEquals(new TextDecoder().decode(out.stdout).trim(), "ok");
  }
});

// RealFSProvider reaches the disk through node:fs, so Deno's read and write
// permissions still apply to every file under its root.
Deno.test("[node/vfs] RealFSProvider is held to Deno's permissions", async () => {
  const root = Deno.realPathSync(Deno.makeTempDirSync());
  Deno.writeTextFileSync(path.join(root, "secret.txt"), "secret");
  const script = `
    import { create, RealFSProvider } from "node:vfs";
    const vfs = create(new RealFSProvider(Deno.args[0]));
    try {
      vfs.readFileSync("/secret.txt", "utf8");
      console.log("read");
    } catch (err) {
      console.log(err.name);
    }
  `;
  const file = scriptFile(script);
  const out = await new Deno.Command(Deno.execPath(), {
    args: ["run", "--no-prompt", file, root],
    stdout: "piped",
    stderr: "piped",
  }).output();
  assertEquals(new TextDecoder().decode(out.stdout).trim(), "NotCapable");

  const allowed = await new Deno.Command(Deno.execPath(), {
    args: ["run", "--no-prompt", `--allow-read=${root}`, file, root],
    stdout: "piped",
    stderr: "piped",
  }).output();
  assertEquals(new TextDecoder().decode(allowed.stdout).trim(), "read");
});

Deno.test("[node/vfs] RealFSProvider keeps paths inside its root", () => {
  const outside = Deno.realPathSync(Deno.makeTempDirSync());
  Deno.writeTextFileSync(path.join(outside, "secret.txt"), "secret");
  const root = Deno.realPathSync(Deno.makeTempDirSync());
  Deno.writeTextFileSync(path.join(root, "in.txt"), "in");
  Deno.symlinkSync(path.join(outside, "secret.txt"), path.join(root, "link"));
  const vfs = create(new RealFSProvider(root));
  assertEquals(vfs.readFileSync("/in.txt", "utf8"), "in");
  // Neither `..` nor a symlink reaches a file outside the root.
  const dotdot = assertThrows(() =>
    vfs.readFileSync(`/../${path.basename(outside)}/secret.txt`)
  ) as NodeJS.ErrnoException;
  assertEquals(dotdot.code, "ENOENT");
  const link = assertThrows(() =>
    vfs.readFileSync("/link")
  ) as NodeJS.ErrnoException;
  assertEquals(link.code, "ENOENT");
});

// Node 26.5.1's RealFSProvider returned a path whose symlink left the root as
// "doesn't exist yet", so these reached the file outside.
Deno.test("[node/vfs] RealFSProvider doesn't write through a symlink out of its root", () => {
  const outside = Deno.realPathSync(Deno.makeTempDirSync());
  const root = Deno.realPathSync(Deno.makeTempDirSync());
  Deno.symlinkSync(outside, path.join(root, "dirlink"));
  Deno.symlinkSync(
    path.join(outside, "created.txt"),
    path.join(root, "dangling"),
  );
  const vfs = create(new RealFSProvider(root));
  for (const target of ["/dirlink/new.txt", "/dangling"]) {
    const err = assertThrows(() =>
      vfs.writeFileSync(target, "x")
    ) as NodeJS.ErrnoException;
    assertEquals(err.code, "ENOENT");
  }
  assertEquals([...Deno.readDirSync(outside)], []);
});
