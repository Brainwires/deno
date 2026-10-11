// Copyright 2018-2026 the Deno authors. MIT license.

// Vitest's snapshot environment (where `.snap` files live and how they are
// read and written), ported from @vitest/snapshot's NodeSnapshotEnvironment
// and vitest's VitestNodeSnapshotEnvironment (MIT, Copyright (c) 2021-Present
// VoidZero Inc. and Vitest contributors).

import { existsSync, promises as fs } from "node:fs";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";

export class DenoSnapshotEnvironment {
  constructor(options = {}) {
    this.options = options;
  }

  getVersion() {
    return "1";
  }

  getHeader() {
    // a Jest project keeps Jest's header (see jest.js)
    return globalThis[Symbol.for("deno.vitest.snapshotHeader")] ??
      `// Vitest Snapshot v${this.getVersion()}, https://vitest.dev/guide/snapshot.html`;
  }

  resolveRawPath(testPath, rawPath) {
    return isAbsolute(rawPath) ? rawPath : resolve(dirname(testPath), rawPath);
  }

  resolvePath(filepath) {
    if (this.options.resolveSnapshotPath) {
      return this.options.resolveSnapshotPath(filepath, ".snap");
    }
    return join(
      join(dirname(filepath), this.options.snapshotsDirName ?? "__snapshots__"),
      `${basename(filepath)}.snap`,
    );
  }

  async prepareDirectory(dirPath) {
    await fs.mkdir(dirPath, { recursive: true });
  }

  async saveSnapshotFile(filepath, snapshot) {
    await fs.mkdir(dirname(filepath), { recursive: true });
    // A Jest project keeps the header of an existing snapshot file (Jest 29
    // and 30 write different ones).
    if (
      globalThis[Symbol.for("deno.vitest.snapshotHeader")] !== undefined &&
      existsSync(filepath)
    ) {
      const existing = (await fs.readFile(filepath, "utf-8")).split("\n", 1)[0];
      const header = /^\/\/ Jest Snapshot v\d/;
      if (header.test(existing) && header.test(snapshot)) {
        snapshot = existing + snapshot.slice(snapshot.indexOf("\n"));
      }
    }
    await fs.writeFile(filepath, snapshot, "utf-8");
  }

  async readSnapshotFile(filepath) {
    if (!existsSync(filepath)) return null;
    return await fs.readFile(filepath, "utf-8");
  }

  async removeSnapshotFile(filepath) {
    if (existsSync(filepath)) await fs.unlink(filepath);
  }
}
