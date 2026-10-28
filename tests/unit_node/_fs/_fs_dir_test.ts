// Copyright 2018-2026 the Deno authors. MIT license.
import { assert, assertEquals, fail } from "@std/assert";
import { assertCallbackErrorUncaught } from "../_test_utils.ts";
import { Dir as DirOrig, type Dirent } from "node:fs";

// deno-lint-ignore no-explicit-any
const Dir = DirOrig as any;

Deno.test({
  name: "Closing current directory with callback is successful",
  async fn() {
    // As in Node, the callback is called asynchronously and close() returns
    // undefined when given one.
    let calledBack = false;
    await new Promise<void>((resolve) => {
      // deno-lint-ignore no-explicit-any
      const ret = new Dir(".").close((valOrErr: any) => {
        assert(!valOrErr);
        calledBack = true;
        resolve();
      });
      assertEquals(ret, undefined);
      assert(!calledBack);
    });
    assert(calledBack);
  },
});

Deno.test({
  name: "Closing current directory without callback returns void Promise",
  async fn() {
    await new Dir(".").close();
  },
});

Deno.test({
  name: "Closing current directory synchronously works",
  fn() {
    new Dir(".").closeSync();
  },
});

Deno.test({
  name: "Path is correctly returned",
  fn() {
    assertEquals(new Dir("std/node").path, "std/node");

    const enc: Uint8Array = new TextEncoder().encode("std/node");
    assertEquals(new Dir(enc).path, "std/node");
  },
});

Deno.test({
  name: "read returns null for empty directory",
  async fn() {
    const testDir: string = Deno.makeTempDirSync();
    try {
      const file: Dirent | null = await new Dir(testDir).read();
      assert(file === null);

      const fileFromCallback = await new Promise<Dirent | null>(
        (resolve) => {
          // deno-lint-ignore no-explicit-any
          new Dir(testDir).read((err: any, res: Dirent | null) => {
            assert(err === null);
            resolve(res);
          });
        },
      );
      assert(fileFromCallback === null);

      assertEquals(new Dir(testDir).readSync(), null);
    } finally {
      Deno.removeSync(testDir);
    }
  },
});

Deno.test({
  name: "Async read returns one file at a time",
  async fn() {
    const testDir: string = Deno.makeTempDirSync();
    const f1 = Deno.createSync(testDir + "/foo.txt");
    f1.close();
    const f2 = Deno.createSync(testDir + "/bar.txt");
    f2.close();

    try {
      let secondCallback = false;
      const dir = new Dir(testDir);
      const firstRead: Dirent | null = await dir.read();
      const secondRead = await new Promise<Dirent | null>((resolve) => {
        // deno-lint-ignore no-explicit-any
        dir.read((_err: any, secondResult: Dirent) => {
          assert(
            secondResult.name === "bar.txt" || secondResult.name === "foo.txt",
          );
          secondCallback = true;
          resolve(secondResult);
        });
      });
      const thirdRead: Dirent | null = await dir.read();
      const fourthRead: Dirent | null = await dir.read();

      if (firstRead?.name === "foo.txt") {
        assertEquals(secondRead?.name, "bar.txt");
      } else if (firstRead?.name === "bar.txt") {
        assertEquals(secondRead?.name, "foo.txt");
      } else {
        fail("File not found during read");
      }
      assert(secondCallback);
      assert(thirdRead === null);
      assert(fourthRead === null);
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});

Deno.test({
  name: "Sync read returns one file at a time",
  fn() {
    const testDir: string = Deno.makeTempDirSync();
    const f1 = Deno.createSync(testDir + "/foo.txt");
    f1.close();
    const f2 = Deno.createSync(testDir + "/bar.txt");
    f2.close();

    try {
      const dir = new Dir(testDir);
      const firstRead: Dirent | null = dir.readSync();
      const secondRead: Dirent | null = dir.readSync();
      const thirdRead: Dirent | null = dir.readSync();
      const fourthRead: Dirent | null = dir.readSync();

      if (firstRead?.name === "foo.txt") {
        assertEquals(secondRead?.name, "bar.txt");
      } else if (firstRead?.name === "bar.txt") {
        assertEquals(secondRead?.name, "foo.txt");
      } else {
        fail("File not found during read");
      }
      assert(thirdRead === null);
      assert(fourthRead === null);
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});

Deno.test({
  name: "Async iteration over existing directory",
  async fn() {
    const testDir: string = Deno.makeTempDirSync();
    const f1 = Deno.createSync(testDir + "/foo.txt");
    f1.close();
    const f2 = Deno.createSync(testDir + "/bar.txt");
    f2.close();

    try {
      const dir = new Dir(testDir);
      const results: Array<string | null> = [];

      for await (const file of dir[Symbol.asyncIterator]()) {
        results.push(file.name);
      }

      assert(results.length === 2);
      assert(results.includes("foo.txt"));
      assert(results.includes("bar.txt"));
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});

Deno.test(
  "[std/node/fs] Dir.close callback isn't called twice if error is thrown",
  async () => {
    const tempDir = await Deno.makeTempDir();
    await assertCallbackErrorUncaught({
      prelude: `
    import { Dir } from "node:fs";

    const dir = new Dir(${JSON.stringify(tempDir)});
    `,
      invocation: "dir.close(",
      async cleanup() {
        await Deno.remove(tempDir);
      },
    });
  },
);

Deno.test(
  "[std/node/fs] Dir.read callback isn't called twice if error is thrown",
  async () => {
    const tempDir = await Deno.makeTempDir();
    await assertCallbackErrorUncaught({
      prelude: `
    import { Dir } from "node:fs";

    const dir = new Dir(${JSON.stringify(tempDir)});
    `,
      invocation: "dir.read(",
      async cleanup() {
        await Deno.remove(tempDir);
      },
    });
  },
);

Deno.test({
  name: "Dir.readSync throws ERR_DIR_CLOSED after closeSync",
  fn() {
    const testDir: string = Deno.makeTempDirSync();
    try {
      const dir = new Dir(testDir);
      dir.closeSync();
      try {
        dir.readSync();
        fail("Expected ERR_DIR_CLOSED to be thrown");
      } catch (e) {
        // deno-lint-ignore no-explicit-any
        assertEquals((e as any).code, "ERR_DIR_CLOSED");
      }
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});

Deno.test({
  name: "Dir.read rejects with ERR_DIR_CLOSED after close",
  async fn() {
    const testDir: string = Deno.makeTempDirSync();
    try {
      const dir = new Dir(testDir);
      await dir.close();
      try {
        await dir.read();
        fail("Expected ERR_DIR_CLOSED to be thrown");
      } catch (e) {
        // deno-lint-ignore no-explicit-any
        assertEquals((e as any).code, "ERR_DIR_CLOSED");
      }
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});

Deno.test({
  name: "Dir.readSync throws ERR_DIR_CLOSED after async close",
  async fn() {
    const testDir: string = Deno.makeTempDirSync();
    try {
      const dir = new Dir(testDir);
      await dir.close();
      try {
        dir.readSync();
        fail("Expected ERR_DIR_CLOSED to be thrown");
      } catch (e) {
        // deno-lint-ignore no-explicit-any
        assertEquals((e as any).code, "ERR_DIR_CLOSED");
      }
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});

Deno.test({
  name: "Dir.read with a callback throws ERR_DIR_CLOSED after close",
  async fn() {
    // Node's Dir#readImpl throws at once on a closed Dir, callback or not.
    const testDir: string = Deno.makeTempDirSync();
    try {
      const dir = new Dir(testDir);
      await dir.close();
      try {
        dir.read(() => fail("callback must not be called"));
        fail("Expected ERR_DIR_CLOSED to be thrown");
      } catch (e) {
        // deno-lint-ignore no-explicit-any
        assertEquals((e as any).code, "ERR_DIR_CLOSED");
      }
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});

Deno.test({
  name: "Dir with recursive: true reads the subdirectories after their parent",
  fn() {
    const testDir: string = Deno.makeTempDirSync();
    Deno.mkdirSync(testDir + "/a/b", { recursive: true });
    Deno.writeTextFileSync(testDir + "/a/b/c.txt", "");
    Deno.writeTextFileSync(testDir + "/d.txt", "");
    try {
      const dir = new Dir(testDir, { recursive: true });
      const names: string[] = [];
      let dirent: Dirent | null;
      while ((dirent = dir.readSync()) !== null) {
        names.push(
          (dirent.parentPath + "/" + dirent.name).slice(testDir.length + 1),
        );
      }
      dir.closeSync();
      assertEquals(names.sort(), ["a", "a/b", "a/b/c.txt", "d.txt"]);
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});

Deno.test({
  name: "Dir.readSync during an asynchronous read throws",
  async fn() {
    const testDir: string = Deno.makeTempDirSync();
    Deno.writeTextFileSync(testDir + "/a.txt", "");
    try {
      const dir = new Dir(testDir);
      const pending = dir.read();
      try {
        dir.readSync();
        fail("Expected ERR_DIR_CONCURRENT_OPERATION to be thrown");
      } catch (e) {
        // deno-lint-ignore no-explicit-any
        assertEquals((e as any).code, "ERR_DIR_CONCURRENT_OPERATION");
      }
      assertEquals((await pending)?.name, "a.txt");
      dir.closeSync();
    } finally {
      Deno.removeSync(testDir, { recursive: true });
    }
  },
});
