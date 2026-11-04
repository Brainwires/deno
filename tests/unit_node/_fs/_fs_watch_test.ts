// Copyright 2018-2026 the Deno authors. MIT license.
import {
  type BigIntStats,
  type Stats,
  unwatchFile,
  watch,
  watchFile,
} from "node:fs";
import { watch as watchPromise } from "node:fs/promises";
import { assert, assertEquals, assertRejects } from "@std/assert";
import { spy } from "@std/testing/mock";

function wait(time: number) {
  return new Promise((resolve) => {
    setTimeout(resolve, time);
  });
}

Deno.test({
  name: "watching a file",
  async fn() {
    const file = Deno.makeTempFileSync();
    const result: Array<[string, string | null]> = [];
    const watcher = watch(
      file,
      (eventType, filename) => result.push([eventType, filename]),
    );
    await wait(100);
    Deno.writeTextFileSync(file, "something");
    await wait(100);
    watcher.close();
    await wait(100);
    assertEquals(result.length >= 1, true);
  },
});

Deno.test({
  name: "watching a file with options",
  // TODO(bartlomieju): this test is flaky on CI
  ignore: true,
  async fn() {
    const file = Deno.makeTempFileSync();
    const spyFn = spy();
    watchFile(
      file,
      { interval: 10 },
      spyFn,
    );
    await wait(100);
    assertEquals(spyFn.calls.length, 0);
    await Deno.writeTextFile(file, "something");
    await wait(100);
    assertEquals(spyFn.calls.length, 1);
    unwatchFile(file);
    await wait(100);
    assertEquals(spyFn.calls.length, 1);
  },
});

Deno.test({
  name: "watchFile detects metadata-only changes (chmod)",
  ignore: Deno.build.os === "windows",
  async fn() {
    const file = Deno.makeTempFileSync();
    Deno.writeTextFileSync(file, "foo");
    Deno.chmodSync(file, 0o644);

    const calls: Array<[Stats, Stats]> = [];
    watchFile(
      file,
      { interval: 10 },
      (curr, prev) => calls.push([curr as Stats, prev as Stats]),
    );

    try {
      await wait(50);
      assertEquals(calls.length, 0);
      Deno.chmodSync(file, 0o600);
      await wait(150);
      assertEquals(calls.length, 1);
      assert(calls[0][0].mode !== calls[0][1].mode);
    } finally {
      unwatchFile(file);
    }
  },
});

Deno.test({
  name: "watchFile honors bigint option",
  async fn() {
    const file = Deno.makeTempFileSync();
    Deno.writeTextFileSync(file, "foo");

    const calls: Array<[BigIntStats, BigIntStats]> = [];
    watchFile(
      file,
      { interval: 10, bigint: true },
      (curr, prev) => calls.push([curr as BigIntStats, prev as BigIntStats]),
    );

    try {
      await wait(50);
      Deno.writeTextFileSync(file, "bar");
      await wait(150);
      assert(calls.length >= 1);
      assertEquals(typeof calls[0][0].mtimeMs, "bigint");
      assertEquals(typeof calls[0][0].ino, "bigint");
    } finally {
      unwatchFile(file);
    }
  },
});

Deno.test({
  name: "watch.unref() should work",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    const file = Deno.makeTempFileSync();
    const watcher = watch(file, () => {});
    // Wait for the watcher to be initialized
    await wait(10);
    // @ts-ignore node types are outdated in deno.
    watcher.unref();
  },
});

Deno.test({
  name: "node [fs/promises] watch should return async iterable",
  sanitizeOps: false,
  sanitizeResources: false,
  async fn() {
    const file = Deno.makeTempFileSync();
    Deno.writeTextFileSync(file, "foo");

    const result: { eventType: string; filename: string | null }[] = [];

    const controller = new AbortController();
    const watcher = watchPromise(file, {
      // Node types resolved by the LSP clash with ours
      // deno-lint-ignore no-explicit-any
      signal: controller.signal as any,
    });

    const deferred = Promise.withResolvers<void>();
    let stopLength = 0;
    setTimeout(async () => {
      Deno.writeTextFileSync(file, "something");
      controller.abort();
      stopLength = result.length;
      await wait(100);
      Deno.writeTextFileSync(file, "something else");
      await wait(100);
      deferred.resolve();
    }, 100);

    // Aborting the signal must surface as an AbortError thrown from the
    // async iterator, matching Node's `fs.promises.watch` behavior.
    await assertRejects(
      async () => {
        for await (const event of watcher) {
          result.push(event);
        }
      },
      Error,
      "The operation was aborted",
    );
    await deferred.promise;

    assertEquals(result.length, stopLength);
    assert(
      result.every((item) =>
        typeof item.eventType === "string" && typeof item.filename === "string"
      ),
    );
  },
});

// Node's `fs.promises.watch` is an async generator: arguments are validated
// and the watcher started by the first `next()`, and an iterator that is never
// consumed holds no watcher (the resource sanitizer checks that here).
Deno.test("[node/fs promises.watch] validates on the first next()", async () => {
  const file = Deno.makeTempFileSync();
  try {
    watchPromise(file);
    const cases: [unknown, unknown, string][] = [
      [1, undefined, "ERR_INVALID_ARG_TYPE"],
      [file, 1, "ERR_INVALID_ARG_TYPE"],
      ["", { persistent: 1 }, "ERR_INVALID_ARG_TYPE"],
      ["", { recursive: 1 }, "ERR_INVALID_ARG_TYPE"],
      ["", { encoding: 1 }, "ERR_INVALID_ARG_VALUE"],
      ["", { encoding: "buffer" }, "ERR_INVALID_ARG_VALUE"],
      ["", { signal: 1 }, "ERR_INVALID_ARG_TYPE"],
      ["", { maxQueue: "silly" }, "ERR_INVALID_ARG_TYPE"],
      ["", { overflow: 1 }, "ERR_INVALID_ARG_VALUE"],
      ["", { overflow: "barf" }, "ERR_INVALID_ARG_VALUE"],
    ];
    for (const [path, options, code] of cases) {
      // deno-lint-ignore no-explicit-any
      const iter = (watchPromise as any)(path, options);
      const err = await assertRejects(() => iter.next());
      assertEquals((err as { code?: string }).code, code);
    }
  } finally {
    Deno.removeSync(file);
  }
});

Deno.test("[node/fs promises.watch] encodes the file name", async () => {
  const dir = Deno.makeTempDirSync();
  try {
    const iter = watchPromise(dir, { encoding: "hex" })[Symbol.asyncIterator]();
    const first = iter.next();
    await wait(100);
    Deno.writeTextFileSync(`${dir}/a`, "x");
    const { value } = await first;
    assertEquals(value?.filename, "61");
    await iter.return?.(undefined);
  } finally {
    Deno.removeSync(dir, { recursive: true });
  }
});

// Runs `code` (an ES module, with the watched path in `FILE`) in a child and
// returns its exit code and stdout. A watcher that keeps the event loop alive
// makes the child run until the 30 s timeout kills it.
async function runWatchChild(code: string, file: string) {
  const { code: exitCode, stdout } = await new Deno.Command(Deno.execPath(), {
    args: [
      "eval",
      "--ext=mjs",
      `const FILE = ${JSON.stringify(file)};\n${code}`,
    ],
    stdout: "piped",
    stderr: "inherit",
    signal: AbortSignal.timeout(30_000),
  }).output();
  return { exitCode, stdout: new TextDecoder().decode(stdout) };
}

Deno.test({
  name:
    "[node/fs promises.watch] persistent: false does not keep the process alive",
  async fn() {
    const file = Deno.makeTempFileSync();
    try {
      const { exitCode, stdout } = await runWatchChild(
        `import { watch } from "node:fs/promises";
const iter = watch(FILE, { persistent: false });
iter.next().then(() => console.log("settled"), () => console.log("rejected"));
console.log("started");`,
        file,
      );
      // As in Node, the pending next() never settles: the process exits.
      assertEquals(exitCode, 0);
      assertEquals(stdout, "started\n");
    } finally {
      Deno.removeSync(file);
    }
  },
});

Deno.test({
  name: "[node/fs watch] persistent: false does not keep the process alive",
  async fn() {
    const file = Deno.makeTempFileSync();
    try {
      const { exitCode, stdout } = await runWatchChild(
        `import { watch } from "node:fs";
watch(FILE, { persistent: false }, () => {});
console.log("started");`,
        file,
      );
      assertEquals(exitCode, 0);
      assertEquals(stdout, "started\n");
    } finally {
      Deno.removeSync(file);
    }
  },
});

Deno.test({
  name: "[node/fs watch] an unref'd watcher stays unref'd after an event",
  async fn() {
    const file = Deno.makeTempFileSync();
    try {
      const { exitCode, stdout } = await runWatchChild(
        `import { watch, writeFileSync } from "node:fs";
let seen = false;
const watcher = watch(FILE, () => {
  if (!seen) console.log("event");
  seen = true;
});
watcher.unref();
setTimeout(() => writeFileSync(FILE, "x"), 200);
setTimeout(() => {}, 1000);`,
        file,
      );
      // The poll started after the event must be unref'd too.
      assertEquals(exitCode, 0);
      assertEquals(stdout, "event\n");
    } finally {
      Deno.removeSync(file);
    }
  },
});

Deno.test({
  name: "[node/fs watchFile] persistent: false does not keep the process alive",
  async fn() {
    const file = Deno.makeTempFileSync();
    try {
      const { exitCode, stdout } = await runWatchChild(
        `import { watchFile } from "node:fs";
watchFile(FILE, { persistent: false, interval: 50 }, () => {});
console.log("started");`,
        file,
      );
      assertEquals(exitCode, 0);
      assertEquals(stdout, "started\n");
    } finally {
      Deno.removeSync(file);
    }
  },
});
