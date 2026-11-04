// deno-lint-ignore-file
// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Node.js contributors. All rights reserved. MIT License.
// Ported from Node.js v26.5.1 (lib/internal/vfs/dir.js).

(function () {
const { core, primordials } = __bootstrap;
const { nextTick } = core.loadExtScript("ext:deno_node/_next_tick.ts");

const {
  SymbolAsyncDispose,
  SymbolAsyncIterator,
  SymbolDispose,
} = primordials;

const {
  ERR_DIR_CLOSED,
} = core.loadExtScript("ext:deno_node/internal/errors.ts");

/**
 * Virtual directory handle returned by VFS opendir/opendirSync.
 * Mimics the subset of the native Dir interface used by Node.js internals
 * (e.g. fs.cp, fs.promises.cp).
 */
class VirtualDir {
  #path;
  #entries;
  #index;
  #closed;

  constructor(dirPath, entries) {
    this.#path = dirPath;
    this.#entries = entries;
    this.#index = 0;
    this.#closed = false;
  }

  get path() {
    return this.#path;
  }

  readSync() {
    if (this.#closed) {
      throw new ERR_DIR_CLOSED();
    }
    if (this.#index >= this.#entries.length) {
      return null;
    }
    return this.#entries[this.#index++];
  }

  async read(callback) {
    if (typeof callback === "function") {
      try {
        const result = this.readSync();
        nextTick(callback, null, result);
      } catch (err) {
        nextTick(callback, err);
      }
      return;
    }
    return this.readSync();
  }

  closeSync() {
    if (this.#closed) {
      throw new ERR_DIR_CLOSED();
    }
    this.#closed = true;
  }

  async close(callback) {
    if (typeof callback === "function") {
      this.closeSync();
      nextTick(callback, null);
      return;
    }
    this.closeSync();
  }

  async *entries() {
    if (this.#closed) {
      throw new ERR_DIR_CLOSED();
    }
    try {
      let entry;
      while ((entry = this.readSync()) !== null) {
        yield entry;
      }
    } finally {
      if (!this.#closed) {
        this.closeSync();
      }
    }
  }

  [SymbolDispose]() {
    if (!this.#closed) {
      this.closeSync();
    }
  }
}

VirtualDir.prototype[SymbolAsyncIterator] = VirtualDir.prototype.entries;
VirtualDir.prototype[SymbolAsyncDispose] = VirtualDir.prototype.close;

return {
  VirtualDir,
};
})();
