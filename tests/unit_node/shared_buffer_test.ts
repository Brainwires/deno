// Copyright 2018-2026 the Deno authors. MIT license.

// Ops never lend a SharedArrayBuffer or a resizable ArrayBuffer to Rust as a
// slice; they work on a copy and write the changed bytes back. These tests
// check that reading into (and writing from) such buffers keeps working.

import { assertEquals, assertNotEquals } from "@std/assert";
import { Buffer } from "node:buffer";
import crypto from "node:crypto";
import fs from "node:fs";
import { Worker } from "node:worker_threads";

const encoder = new TextEncoder();

function shared(length: number): Uint8Array {
  return new Uint8Array(new SharedArrayBuffer(length));
}

function resizable(length: number): Uint8Array {
  return new Uint8Array(
    new ArrayBuffer(length, { maxByteLength: length * 2 }),
  );
}

const backings = [["shared", shared], ["resizable", resizable]] as const;

function withFile(contents: string, fn: (path: string) => void) {
  const path = Deno.makeTempFileSync();
  try {
    Deno.writeTextFileSync(path, contents);
    fn(path);
  } finally {
    Deno.removeSync(path);
  }
}

for (const [name, make] of backings) {
  Deno.test(`[node/fs] readSync into a ${name} buffer`, () => {
    withFile("hello world", (path) => {
      const view = make(16);
      view[15] = 42;
      const buf = Buffer.from(view.buffer, view.byteOffset, view.byteLength);
      const fd = fs.openSync(path, "r");
      try {
        assertEquals(fs.readSync(fd, buf, 2, 5, 0), 5);
      } finally {
        fs.closeSync(fd);
      }
      assertEquals(buf.subarray(2, 7).toString(), "hello");
      assertEquals(view[0], 0);
      assertEquals(view[15], 42);
    });
  });

  Deno.test(`[node/fs] readvSync into ${name} buffers`, () => {
    withFile("hello world", (path) => {
      const a = make(5);
      const b = make(6);
      const fd = fs.openSync(path, "r");
      try {
        assertEquals(fs.readvSync(fd, [a, b], 0), 11);
      } finally {
        fs.closeSync(fd);
      }
      assertEquals(new TextDecoder().decode(a.slice()), "hello");
      assertEquals(new TextDecoder().decode(b.slice()), " world");
    });
  });

  Deno.test(`[node/fs] read (callback) into a ${name} buffer`, async () => {
    const path = Deno.makeTempFileSync();
    try {
      Deno.writeTextFileSync(path, "async read");
      const view = make(10);
      const fd = fs.openSync(path, "r");
      try {
        const bytesRead = await new Promise<number>((resolve, reject) =>
          fs.read(
            fd,
            view,
            0,
            10,
            0,
            (err, n) => err ? reject(err) : resolve(n),
          )
        );
        assertEquals(bytesRead, 10);
      } finally {
        fs.closeSync(fd);
      }
      assertEquals(new TextDecoder().decode(view.slice()), "async read");
    } finally {
      Deno.removeSync(path);
    }
  });

  Deno.test(`[node/fs] writeSync from a ${name} buffer`, () => {
    const path = Deno.makeTempFileSync();
    try {
      const view = make(5);
      view.set(encoder.encode("abcde"));
      const fd = fs.openSync(path, "w");
      try {
        assertEquals(fs.writeSync(fd, view), 5);
      } finally {
        fs.closeSync(fd);
      }
      assertEquals(Deno.readTextFileSync(path), "abcde");
    } finally {
      Deno.removeSync(path);
    }
  });

  Deno.test(`[node/crypto] randomFillSync into a ${name} buffer`, () => {
    const view = make(64);
    crypto.randomFillSync(view);
    assertNotEquals(view.slice(), new Uint8Array(64));
  });

  Deno.test(`[node/crypto] randomFill into a ${name} buffer`, async () => {
    const view = make(64);
    await new Promise<void>((resolve, reject) =>
      crypto.randomFill(view, (err) => err ? reject(err) : resolve())
    );
    assertNotEquals(view.slice(), new Uint8Array(64));
  });

  Deno.test(`[web] TextEncoder.encodeInto a ${name} buffer`, () => {
    const view = make(8);
    view[7] = 1;
    const result = encoder.encodeInto("héllo", view);
    assertEquals(result, { read: 5, written: 6 });
    assertEquals(new TextDecoder().decode(view.slice(0, 6)), "héllo");
    assertEquals(view[7], 1);
  });

  Deno.test(`[deno] Deno.readSync into a ${name} buffer`, () => {
    withFile("deno read", (path) => {
      const view = make(9);
      using file = Deno.openSync(path);
      assertEquals(file.readSync(view), 9);
      assertEquals(new TextDecoder().decode(view.slice()), "deno read");
    });
  });
}

Deno.test("[node/crypto] randomFillSync into a SharedArrayBuffer", () => {
  const sab = new SharedArrayBuffer(32);
  crypto.randomFillSync(sab);
  assertNotEquals(new Uint8Array(sab).slice(), new Uint8Array(32));
});

Deno.test("[node/fs] readSync into a resizable buffer that shrank", () => {
  withFile("0123456789", (path) => {
    const ab = new ArrayBuffer(10, { maxByteLength: 20 });
    ab.resize(4);
    const fd = fs.openSync(path, "r");
    try {
      assertEquals(fs.readSync(fd, new Uint8Array(ab), 0, 4, 0), 4);
    } finally {
      fs.closeSync(fd);
    }
    assertEquals(new TextDecoder().decode(new Uint8Array(ab)), "0123");
  });
});

Deno.test(
  "[node/worker_threads] reads into a SharedArrayBuffer a worker watches",
  async () => {
    const sab = new SharedArrayBuffer(16);
    const flags = new Int32Array(sab, 0, 2);
    const data = new Uint8Array(sab, 8, 8);
    const worker = new Worker(
      `
      const { workerData, parentPort } = require("node:worker_threads");
      const flags = new Int32Array(workerData, 0, 2);
      const data = new Uint8Array(workerData, 8, 8);
      Atomics.store(flags, 1, 1);
      Atomics.notify(flags, 1);
      Atomics.wait(flags, 0, 0);
      parentPort.postMessage(new TextDecoder().decode(data.slice()));
      `,
      { eval: true, workerData: sab },
    );
    try {
      const message = new Promise((resolve) => worker.once("message", resolve));
      // The worker is running and waiting on flags[0].
      while (Atomics.load(flags, 1) !== 1) {
        await new Promise((resolve) => setTimeout(resolve, 5));
      }
      withFile("from fs!", (path) => {
        const fd = fs.openSync(path, "r");
        try {
          assertEquals(fs.readSync(fd, data, 0, 8, 0), 8);
        } finally {
          fs.closeSync(fd);
        }
      });
      Atomics.store(flags, 0, 1);
      Atomics.notify(flags, 0);
      assertEquals(await message, "from fs!");
      // The read did not overwrite the flags the worker set.
      assertEquals(Atomics.load(flags, 1), 1);
    } finally {
      await worker.terminate();
    }
  },
);
