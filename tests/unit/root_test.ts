// Copyright 2018-2026 the Deno authors. MIT license.

// denext: `Deno.openRoot()` (ext/fs/root.rs).

import {
  assert,
  assertEquals,
  assertRejects,
  assertThrows,
} from "./test_util.ts";
import { join } from "@std/path";

const windows = Deno.build.os === "windows";

/** `<tmp>/root` (opened as the root) beside `<tmp>/outside/secret.txt`. */
function bed() {
  const tmp = Deno.makeTempDirSync({ prefix: "deno_root_test_" });
  const root = join(tmp, "root");
  const outside = join(tmp, "outside");
  Deno.mkdirSync(root);
  Deno.mkdirSync(outside);
  Deno.writeTextFileSync(join(outside, "secret.txt"), "outside");
  return {
    tmp,
    root,
    outside,
    [Symbol.dispose]() {
      Deno.removeSync(tmp, { recursive: true });
    },
  };
}

/** A link to a directory (a junction on Windows, which needs no privilege). */
function linkDir(target: string, path: string) {
  Deno.symlinkSync(target, path, { type: windows ? "junction" : "dir" });
}

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootReadsAndWritesBeneathItself() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    assertEquals(root.path, b.root);

    await root.writeTextFile("a.txt", "hello");
    assertEquals(await root.readTextFile("a.txt"), "hello");
    assertEquals(Deno.readTextFileSync(join(b.root, "a.txt")), "hello");
    root.writeFileSync("b.bin", new Uint8Array([1, 2, 3]));
    assertEquals(root.readFileSync("b.bin"), new Uint8Array([1, 2, 3]));
    await root.writeTextFile("a.txt", " world", { append: true });
    assertEquals(root.readTextFileSync("a.txt"), "hello world");
    await assertRejects(
      () => root.writeTextFile("a.txt", "x", { createNew: true }),
      Deno.errors.AlreadyExists,
    );
    await assertRejects(
      () => root.writeTextFile("missing.txt", "x", { create: false }),
      Deno.errors.NotFound,
    );

    await root.mkdir("d/e/f", { recursive: true });
    root.mkdirSync("d/e/f", { recursive: true });
    await assertRejects(() => root.mkdir("d"), Deno.errors.AlreadyExists);
    root.writeTextFileSync("d/e/f/g.txt", "deep");
    // `..` that stays inside is resolved lexically
    assertEquals(await root.readTextFile("d/e/../e/f/g.txt"), "deep");
    assertEquals(await root.readTextFile("./d/e/f/g.txt"), "deep");

    const st = await root.stat("d/e/f/g.txt");
    assert(st.isFile);
    assertEquals(st.size, 4);
    assert(root.statSync("d").isDirectory);
    assert((await root.stat("")).isDirectory);
    assert(root.lstatSync(".").isDirectory);

    const names = [];
    for await (const entry of root.readDir(".")) names.push(entry.name);
    names.sort();
    assertEquals(names, ["a.txt", "b.bin", "d"]);
    const entries = [...root.readDirSync("d")];
    assertEquals(entries.length, 1);
    assertEquals(entries[0].name, "e");
    assert(entries[0].isDirectory);

    await root.rename("a.txt", "d/moved.txt");
    root.renameSync("d/moved.txt", "d/e/moved.txt");
    assertEquals(root.readTextFileSync("d/e/moved.txt"), "hello world");
    await assertRejects(() => root.stat("a.txt"), Deno.errors.NotFound);

    {
      using file = await root.open("d/e/f/g.txt");
      const buf = new Uint8Array(4);
      assertEquals(await file.read(buf), 4);
      assertEquals(new TextDecoder().decode(buf), "deep");
    }
    {
      using file = root.openSync("new.txt", { write: true, createNew: true });
      file.writeSync(new TextEncoder().encode("made"));
    }
    assertEquals(root.readTextFileSync("new.txt"), "made");
    {
      using file = await root.create("new.txt");
      assertEquals((await file.stat()).size, 0);
    }

    await root.remove("b.bin");
    await assertRejects(() => root.remove("d"), Error);
    root.removeSync("d", { recursive: true });
    assertThrows(() => root.statSync("d"), Deno.errors.NotFound);
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  function rootRefusesPathsThatEscape() {
    using b = bed();
    using root = Deno.openRootSync(b.root);
    const escapes = [
      "../outside/secret.txt",
      "..",
      "a/../../outside/secret.txt",
      join(b.outside, "secret.txt"),
      "/etc/passwd",
    ];
    if (windows) {
      escapes.push("C:\\Windows\\win.ini", "\\outside", "C:outside");
    }
    for (const path of escapes) {
      assertThrows(() => root.readTextFileSync(path), Deno.errors.NotCapable);
      assertThrows(() => root.statSync(path), Deno.errors.NotCapable);
      assertThrows(
        () => root.writeTextFileSync(path, "pwned"),
        Deno.errors.NotCapable,
      );
      assertThrows(() => root.mkdirSync(path), Deno.errors.NotCapable);
      assertThrows(() => root.removeSync(path), Deno.errors.NotCapable);
      assertThrows(() => [...root.readDirSync(path)], Deno.errors.NotCapable);
    }
    assertEquals(
      Deno.readTextFileSync(join(b.outside, "secret.txt")),
      "outside",
    );
    assertThrows(
      // deno-lint-ignore no-explicit-any
      () => root.readTextFileSync(new URL("file:///etc/passwd") as any),
      TypeError,
    );
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootRefusesARenameOut() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    await root.writeTextFile("a.txt", "inside");
    await assertRejects(
      () => root.rename("a.txt", "../a.txt"),
      Deno.errors.NotCapable,
    );
    assertThrows(
      () => root.renameSync("../outside/secret.txt", "stolen.txt"),
      Deno.errors.NotCapable,
    );
    assertThrows(() => root.renameSync(".", "x"), Deno.errors.NotCapable);
    // the root itself can't be removed through it
    await assertRejects(
      () => root.remove(".", { recursive: true }),
      Deno.errors.NotCapable,
    );
    assertEquals(root.readTextFileSync("a.txt"), "inside");
    assertEquals(
      Deno.readTextFileSync(join(b.outside, "secret.txt")),
      "outside",
    );
    assertThrows(
      () => Deno.statSync(join(b.tmp, "a.txt")),
      Deno.errors.NotFound,
    );
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootNeverFollowsALinkOut() {
    using b = bed();
    linkDir(b.outside, join(b.root, "out"));
    using root = await Deno.openRoot(b.root);

    await assertRejects(
      () => root.readTextFile("out/secret.txt"),
      Deno.errors.NotCapable,
    );
    await assertRejects(
      () => root.writeTextFile("out/planted.txt", "x"),
      Deno.errors.NotCapable,
    );
    await assertRejects(() => root.stat("out"), Deno.errors.NotCapable);
    assertThrows(() => [...root.readDirSync("out")], Deno.errors.NotCapable);
    assertThrows(
      () => root.mkdirSync("out/sub", { recursive: true }),
      Deno.errors.NotCapable,
    );
    assertThrows(() => Deno.statSync(join(b.outside, "planted.txt")));
    assertThrows(() => Deno.statSync(join(b.outside, "sub")));

    // lstat sees the link itself
    assert((await root.lstat("out")).isSymlink);
    // a recursive remove of a folder holding the link removes the link, not
    // what it leads to
    await root.mkdir("holder");
    linkDir(b.outside, join(b.root, "holder", "out"));
    await root.remove("holder", { recursive: true });
    assertEquals(
      Deno.readTextFileSync(join(b.outside, "secret.txt")),
      "outside",
    );

    if (!windows) {
      // a file link out, absolute and relative
      Deno.symlinkSync(join(b.outside, "secret.txt"), join(b.root, "abs"));
      Deno.symlinkSync("../outside/secret.txt", join(b.root, "rel"));
      for (const link of ["abs", "rel"]) {
        assertThrows(() => root.readTextFileSync(link), Deno.errors.NotCapable);
        assertThrows(
          () => root.writeTextFileSync(link, "pwned"),
          Deno.errors.NotCapable,
        );
      }
      // a symlink made through the root to outside leads nowhere either
      await root.symlink("../outside", "made");
      assertEquals(await root.readLink("made"), "../outside");
      await assertRejects(
        () => root.readTextFile("made/secret.txt"),
        Deno.errors.NotCapable,
      );
      assertEquals(
        Deno.readTextFileSync(join(b.outside, "secret.txt")),
        "outside",
      );
    }
  },
);

Deno.test(
  { ignore: windows, permissions: { read: true, write: true } },
  async function rootFollowsALinkThatStaysInside() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    await root.mkdir("real");
    await root.writeTextFile("real/f.txt", "inside");
    await root.symlink("real", "alias");
    await root.symlink(join(b.root, "real", "f.txt"), "abs-alias");
    assertEquals(await root.readTextFile("alias/f.txt"), "inside");
    assert((await root.stat("alias")).isDirectory);
    assertEquals(await root.readTextFile("abs-alias"), "inside");
  },
);

Deno.test(
  { ignore: !windows, permissions: { read: true, write: true } },
  async function rootRefusesAJunctionInsideOnWindows() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    await root.mkdir("real");
    await root.writeTextFile("real/f.txt", "inside");
    linkDir(join(b.root, "real"), join(b.root, "alias"));
    await assertRejects(
      () => root.readTextFile("alias/f.txt"),
      Deno.errors.NotCapable,
    );
    await assertRejects(
      () => root.symlink("real", "x"),
      Deno.errors.NotSupported,
    );
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootHoldsItsDirectoryWhenThePathIsSwapped() {
    using b = bed();
    using root = await Deno.openRoot(b.root);
    await root.writeTextFile("f.txt", "original");
    // the root's own path is moved away and replaced by a link out
    Deno.renameSync(b.root, join(b.tmp, "moved"));
    linkDir(b.outside, b.root);
    assertEquals(await root.readTextFile("f.txt"), "original");
    await assertRejects(
      () => root.readTextFile("secret.txt"),
      Deno.errors.NotFound,
    );
    await root.writeTextFile("g.txt", "written");
    assertEquals(
      Deno.readTextFileSync(join(b.tmp, "moved", "g.txt")),
      "written",
    );
    assertThrows(() => Deno.statSync(join(b.outside, "g.txt")));
  },
);

Deno.test(
  { permissions: { read: true, write: true } },
  async function rootSurvivesASymlinkSwapRace() {
    using b = bed();
    Deno.mkdirSync(join(b.root, "sub"));
    Deno.writeTextFileSync(join(b.root, "sub", "secret.txt"), "inside");
    using root = await Deno.openRoot(b.root);

    // a worker swaps `sub` between the real folder and a link out, as fast
    // as it can, while the root reads through it
    const code = `
      const [sub, real, outside, windows] = ${
      JSON.stringify([
        join(b.root, "sub"),
        join(b.root, "sub.real"),
        b.outside,
        windows,
      ])
    };
      let stop = false;
      self.onmessage = () => { stop = true; };
      // swaps that went all the way: the link out stood where the folder was
      let swaps = 0;
      for (let tries = 0; !stop && tries < 20000; tries++) {
        let moved = false;
        try {
          Deno.renameSync(sub, real);
          moved = true;
          Deno.symlinkSync(outside, sub, { type: windows ? "junction" : "dir" });
          Deno.removeSync(sub);
          swaps++;
        } catch { /* a read holds a handle (Windows): try again */ }
        // the folder goes back however far the swap got
        for (let i = 0; moved && i < 1000; i++) {
          try {
            Deno.renameSync(real, sub);
            moved = false;
          } catch {
            try { Deno.removeSync(sub); } catch { /* no link left */ }
          }
        }
        if (tries % 64 === 63) await new Promise((r) => setTimeout(r, 0));
      }
      self.postMessage(swaps);
    `;
    const worker = new Worker(
      URL.createObjectURL(new Blob([code], { type: "text/javascript" })),
      { type: "module" },
    );
    const done = new Promise<number>((resolve) => {
      worker.onmessage = (e) => resolve(e.data);
    });
    // what each read returned, when it was not the file inside: anything
    // here was read from outside the root
    const escaped: string[] = [];
    let inside = 0;
    let swaps = 0;
    // a refusal (the link), or a read caught between the swap's steps (the
    // name gone, a link where the walk wanted a folder, a link gone again
    // before the walk read it: EINVAL) is fine; anything that isn't an
    // error is not
    const refusal = (err: unknown) => assert(err instanceof Error, String(err));
    try {
      // at least 2000 reads, and on until one got through to the folder
      // (the worker stops swapping after its 20000 tries)
      for (let i = 0; i < 2000 || (inside === 0 && i < 200000); i++) {
        let text: string | undefined;
        try {
          text = await root.readTextFile("sub/secret.txt");
        } catch (err) {
          refusal(err);
        }
        if (text === "inside") inside++;
        else if (text !== undefined) escaped.push(text);
        // the same race for a write: it lands in the folder or nowhere
        try {
          await root.writeTextFile("sub/written.txt", "from the root");
        } catch (err) {
          refusal(err);
        }
      }
    } finally {
      worker.postMessage("stop");
      swaps = await done;
      worker.terminate();
    }
    // tidy the bed if the worker stopped half way through a swap
    try {
      Deno.statSync(join(b.root, "sub.real"));
      try {
        Deno.removeSync(join(b.root, "sub"));
      } catch { /* none */ }
      Deno.renameSync(join(b.root, "sub.real"), join(b.root, "sub"));
    } catch { /* already back */ }

    assertEquals(escaped, [], "read outside the root");
    assertEquals(
      [...Deno.readDirSync(b.outside)].map((entry) => entry.name),
      ["secret.txt"],
      "wrote outside the root",
    );
    assertEquals(
      Deno.readTextFileSync(join(b.outside, "secret.txt")),
      "outside",
    );
    // the race was run: the link out stood in the folder's place, and the
    // root read through the folder
    assert(swaps > 0, "the worker never swapped the folder for the link");
    assert(inside > 0, "no read reached the folder");
  },
);

Deno.test(
  { ignore: windows, permissions: { read: true, write: true } },
  async function rootLinksStayInsideUnderScopedGrants() {
    using b = bed();
    const secret = join(b.outside, "secret.txt");
    Deno.mkdirSync(join(b.root, "sub"));
    Deno.writeTextFileSync(join(b.root, "f.txt"), "inside");
    // links that were there before: one that stays inside (to the root
    // itself), one that leads out
    Deno.symlinkSync("..", join(b.root, "sub", "up"));
    Deno.symlinkSync("../outside", join(b.root, "out"));

    // [target, path]: each would lead out of the root when followed
    const refused = [
      ["../outside/secret.txt", "a"],
      [secret, "b"],
      ["sub/../../outside/secret.txt", "c"],
      ["../../outside/secret.txt", "sub/d"],
      // `sub/up` is the root: from there `..` is outside
      ["../outside/secret.txt", "sub/up/e"],
      // through a link that leads out
      ["out/secret.txt", "f"],
    ];
    const allowed = [
      ["f.txt", "h"],
      ["./f.txt", "i"],
      ["../f.txt", "sub/j"],
      ["missing/later.txt", "k"],
      [".", "l"],
    ];
    // a program with grants scoped to the root's folder: a link to anywhere
    // takes unscoped read and write, as `Deno.symlink()` does
    const code = `
      const [dir, refused, allowed] = ${
      JSON.stringify([b.root, refused, allowed])
    };
      const result = { refused: [], allowed: [], byName: "" };
      const root = Deno.openRootSync(dir);
      let n = 0;
      for (const [target, path] of refused) {
        try {
          if (n++ % 2) root.symlinkSync(target, path);
          else await root.symlink(target, path);
          result.refused.push("made");
        } catch (err) {
          result.refused.push(err.name);
        }
      }
      for (const [target, path] of allowed) {
        try {
          if (n++ % 2) root.symlinkSync(target, path);
          else await root.symlink(target, path);
          result.allowed.push("made");
        } catch (err) {
          result.allowed.push(err.name + ": " + err.message);
        }
      }
      try {
        Deno.symlinkSync("f.txt", dir + "/m");
        result.byName = "made";
      } catch (err) {
        result.byName = err.name;
      }
      root.close();
      self.postMessage(result);
    `;
    const worker = new Worker(
      URL.createObjectURL(new Blob([code], { type: "text/javascript" })),
      {
        type: "module",
        deno: { permissions: { read: [b.root], write: [b.root] } },
      },
    );
    const result = await new Promise<{
      refused: string[];
      allowed: string[];
      byName: string;
    }>((resolve, reject) => {
      worker.onmessage = (e) => resolve(e.data);
      worker.onerror = (e) => reject(e.error ?? new Error(e.message));
    });
    worker.terminate();

    assertEquals(result.refused, refused.map(() => "NotCapable"));
    for (const [, path] of refused) {
      assertThrows(
        () => Deno.lstatSync(join(b.root, path)),
        Deno.errors.NotFound,
      );
    }
    assertEquals(result.allowed, allowed.map(() => "made"));
    for (const [target, path] of allowed) {
      assertEquals(Deno.readLinkSync(join(b.root, path)), target);
    }
    assertEquals(result.byName, "NotCapable");
    assertEquals(Deno.readTextFileSync(secret), "outside");
  },
);

Deno.test(
  { ignore: windows, permissions: { read: true, write: true } },
  async function rootHoldsTheKernelTreesToTheSpecialFileRule() {
    // unscoped read and write, and nothing else: what `Deno.open()` asks
    // every permission for by name, it asks for through a root too
    using b = bed();
    using root = await Deno.openRoot("/");
    const beneath = (path: string) => path.replace(/^\/+/, "");

    // needs no grant by name, so none here
    assertEquals(root.readFileSync("dev/null").length, 0);
    assertEquals((await root.readFile("dev/null")).length, 0);

    const refused = ["dev/stdin"];
    if (Deno.build.os === "linux") {
      refused.push(
        "proc/self/environ",
        `proc/${Deno.pid}/environ`,
        "proc/self/mem",
        "proc/self/root",
        "tmp/../proc/self/environ",
      );
    }
    for (const path of refused) {
      assertThrows(() => Deno.lstatSync("/" + path), Deno.errors.NotCapable);
      assertThrows(
        () => root.lstatSync(path),
        Deno.errors.NotCapable,
        undefined,
        path,
      );
      await assertRejects(
        () => root.lstat(path),
        Deno.errors.NotCapable,
        undefined,
        path,
      );
      assertThrows(
        () => root.openSync(path),
        Deno.errors.NotCapable,
        undefined,
        path,
      );
      await assertRejects(
        () => root.open(path, { read: true, write: true }),
        Deno.errors.NotCapable,
        undefined,
        path,
      );
      assertThrows(
        () => root.readFileSync(path),
        Deno.errors.NotCapable,
        undefined,
        path,
      );
      assertThrows(
        () => root.writeFileSync(path, new Uint8Array([0])),
        Deno.errors.NotCapable,
        undefined,
        path,
      );
      // a link in the root that leads there is refused as a link out is
      const link = join(b.root, path.replaceAll(/[^a-z]/g, "_"));
      Deno.symlinkSync("/" + path.replace("tmp/../", ""), link);
      assertThrows(
        () => root.readFileSync(beneath(link)),
        Deno.errors.NotCapable,
        undefined,
        path,
      );
      await assertRejects(
        () => root.open(beneath(link), { write: true }),
        Deno.errors.NotCapable,
        undefined,
        path,
      );
    }
    // a folder link, with the tree's file named after it
    Deno.symlinkSync("/dev", join(b.root, "devices"));
    assertThrows(
      () => root.lstatSync(beneath(join(b.root, "devices", "stdin"))),
      Deno.errors.NotCapable,
    );

    if (Deno.build.os === "linux") {
      assertThrows(
        () => root.readDirSync("proc/self/fd"),
        Deno.errors.NotCapable,
      );
      // nor is a root opened on one of them a way in
      await assertRejects(
        () => Deno.openRoot("/proc/self/root"),
        Deno.errors.NotCapable,
      );
      assertThrows(
        () => Deno.openRootSync("/proc/self/root"),
        Deno.errors.NotCapable,
      );
    }
    await assertRejects(() => Deno.openRoot("/dev"), Deno.errors.NotCapable);
  },
);

Deno.test(
  { ignore: windows },
  function rootReachesTheKernelTreesWithEveryPermission() {
    using root = Deno.openRootSync("/");
    assertEquals(root.readFileSync("dev/null").length, 0);
    assert(root.lstatSync("dev/stdin") !== null);
  },
);

Deno.test(
  { permissions: { read: true, write: false } },
  async function rootWritesNeedWriteAccess() {
    using root = await Deno.openRoot(".");
    await assertRejects(
      () => root.writeTextFile("denext_root_test_never.txt", "x"),
      Deno.errors.NotCapable,
    );
    assertThrows(() => root.mkdirSync("never"), Deno.errors.NotCapable);
    assertThrows(() => root.removeSync("never"), Deno.errors.NotCapable);
  },
);

Deno.test({ permissions: { read: false } }, async function rootNeedsRead() {
  await assertRejects(() => Deno.openRoot("."), Deno.errors.NotCapable);
  assertThrows(() => Deno.openRootSync("."), Deno.errors.NotCapable);
});

Deno.test(
  { permissions: { read: true } },
  async function rootClosesAndCannotBeConstructed() {
    const root = await Deno.openRoot(".");
    root.close();
    assertThrows(() => root.statSync("."), Deno.errors.BadResource);
    assertThrows(
      // deno-lint-ignore no-explicit-any
      () => new (Deno.Root as any)(0, ".", undefined),
      TypeError,
    );
    await assertRejects(
      () => Deno.openRoot("./denext-root-test-missing"),
      Deno.errors.NotFound,
    );
  },
);
