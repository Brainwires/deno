// Copyright 2018-2026 the Deno authors. MIT license.

import { core, primordials } from "ext:core/mod.js";
import {
  type Dirent,
  direntFromDeno,
  getOptions,
} from "ext:deno_node/internal/fs/utils.mjs";
const {
  ERR_DIR_CLOSED,
  ERR_DIR_CONCURRENT_OPERATION,
  ERR_INVALID_THIS,
  ERR_MISSING_ARGS,
  denoErrorToNodeError,
} = core.loadExtScript("ext:deno_node/internal/errors.ts");
const { validateFunction, validateUint32 } = core.loadExtScript(
  "ext:deno_node/internal/validators.mjs",
);
const { join } = core.loadExtScript("ext:deno_node/path/mod.ts");
const { TextDecoder } = core.loadExtScript("ext:deno_web/08_text_encoding.js");
const lazyProcess = core.createLazyLoader("node:process");

const {
  ArrayFrom,
  ArrayPrototypePush,
  ArrayPrototypeShift,
  ObjectDefineProperties,
  ObjectPrototypeIsPrototypeOf,
  Promise,
  PromisePrototypeThen,
  PromiseReject,
  PromiseResolve,
  ReflectApply,
  SafeArrayIterator,
  SymbolAsyncDispose,
  SymbolAsyncIterator,
  SymbolDispose,
  Uint8ArrayPrototype,
} = primordials;

// A directory being read: Deno reads a directory's entries in one call, so a
// "handle" holds them once read and hands them out `bufferSize` at a time.
type DirHandle = {
  path: string;
  entries: Deno.DirEntry[] | null;
  index: number;
};

function readEntriesSync(path: string, syscall: string): Deno.DirEntry[] {
  try {
    return ArrayFrom(Deno.readDirSync(path));
  } catch (err) {
    throw denoErrorToNodeError(err as Error, { syscall, path });
  }
}

function readHandleSync(
  handle: DirHandle,
  count: number,
): Deno.DirEntry[] | null {
  handle.entries ??= readEntriesSync(handle.path, "scandir");
  return takeEntries(handle, count);
}

function takeEntries(handle: DirHandle, count: number) {
  const entries = handle.entries!;
  if (handle.index >= entries.length) {
    return null;
  }
  const batch = [];
  while (handle.index < entries.length && batch.length < count) {
    ArrayPrototypePush(batch, entries[handle.index++]);
  }
  return batch;
}

async function readHandle(
  handle: DirHandle,
  count: number,
  syscall = "scandir",
) {
  if (handle.entries === null) {
    const entries: Deno.DirEntry[] = [];
    try {
      const iterator = Deno.readDir(handle.path)[SymbolAsyncIterator]();
      while (true) {
        const result = await ReflectApply(iterator.next, iterator, []);
        if (result.done) break;
        ArrayPrototypePush(entries, result.value);
      }
    } catch (err) {
      throw denoErrorToNodeError(err as Error, { syscall, path: handle.path });
    }
    handle.entries = entries;
  }
  return takeEntries(handle, count);
}

/**
 * Opens the directory at `path` the way `uv_fs_opendir` does: a missing path
 * or a file is an `opendir` error at open time. The entries read to check it
 * are kept, so the directory is read once.
 */
export function openDirHandleSync(path: string): DirHandle {
  return { path, entries: readEntriesSync(path, "opendir"), index: 0 };
}

export function openDirHandle(path: string): Promise<DirHandle> {
  const handle: DirHandle = { path, entries: null, index: 0 };
  return PromisePrototypeThen(readHandle(handle, 0, "opendir"), () => handle);
}

// Note: unlike `fs.readdir`, `fs.opendir`/`Dir` streams entries in filesystem
// order and does NOT sort them, matching Node.js (libuv's `uv_fs_readdir`, as
// opposed to `uv_fs_scandir` which backs `readdir`). Do not add sorting here.
//
// The class follows Node's `lib/internal/fs/dir.js`: entries are buffered
// `bufferSize` at a time, an asynchronous read that has to go to the file
// system makes a concurrent `readSync()` / `closeSync()` throw
// `ERR_DIR_CONCURRENT_OPERATION` (callbacks queue behind it), a closed `Dir`
// throws `ERR_DIR_CLOSED`, and `recursive: true` queues each directory entry it
// returns to be read after the entries already buffered.
export default class Dir {
  #dirPath: string | Uint8Array;
  #handle: DirHandle | null;
  #bufferedEntries: Dirent[] = [];
  #closed = false;
  // deno-lint-ignore no-explicit-any
  #options: any;
  #operationQueue: (() => void)[] | null = null;
  #handlerQueue: DirHandle[] = [];

  constructor(
    path: string | Uint8Array,
    // deno-lint-ignore no-explicit-any
    options?: any,
    handle?: DirHandle,
  ) {
    if (!path) {
      throw new ERR_MISSING_ARGS("path");
    }
    this.#dirPath = path;
    this.#options = {
      bufferSize: 32,
      ...getOptions(options, { encoding: "utf8" }),
    };
    validateUint32(this.#options.bufferSize, "options.bufferSize", true);
    this.#handle = handle ??
      { path: this.#pathString(), entries: null, index: 0 };
  }

  #pathString(): string {
    if (ObjectPrototypeIsPrototypeOf(Uint8ArrayPrototype, this.#dirPath)) {
      return new TextDecoder().decode(this.#dirPath as Uint8Array);
    }
    return this.#dirPath as string;
  }

  get path(): string {
    if (!(#dirPath in this)) {
      throw new ERR_INVALID_THIS("Dir");
    }
    return this.#pathString();
  }

  #processReadResult(
    parentPath: string | Uint8Array,
    entries: Deno.DirEntry[],
  ) {
    for (const entry of new SafeArrayIterator(entries)) {
      ArrayPrototypePush(
        this.#bufferedEntries,
        direntFromDeno(entry, parentPath),
      );
    }
  }

  #processHandlerQueue(): boolean {
    while (this.#handlerQueue.length > 0) {
      const handle = ArrayPrototypeShift(this.#handlerQueue)!;
      const result = readHandleSync(handle, this.#options.bufferSize);
      if (result !== null) {
        this.#processReadResult(handle.path, result);
        if (result.length > 0) {
          ArrayPrototypePush(this.#handlerQueue, handle);
        }
      }
      if (this.#bufferedEntries.length > 0) {
        break;
      }
    }
    return this.#bufferedEntries.length > 0;
  }

  #readSyncRecursive(dirent: Dirent) {
    const path = join(dirent.parentPath, dirent.name);
    ArrayPrototypePush(this.#handlerQueue, openDirHandleSync(path));
  }

  #shiftEntry(): Dirent {
    const dirent = ArrayPrototypeShift(this.#bufferedEntries)!;
    if (this.#options.recursive && dirent.isDirectory()) {
      this.#readSyncRecursive(dirent);
    }
    return dirent;
  }

  // deno-lint-ignore no-explicit-any
  read(callback?: (...args: any[]) => void): Promise<Dirent | null> | void {
    if (arguments.length === 0) {
      return this.#readPromisified();
    }
    return this.#readImpl(true, callback);
  }

  #readPromisified(): Promise<Dirent | null> {
    return new Promise((resolve, reject) => {
      try {
        this.#readImpl(false, (err, dirent) => {
          if (err) reject(err);
          else resolve(dirent);
        });
      } catch (err) {
        reject(err);
      }
    });
  }

  // deno-lint-ignore no-explicit-any
  #readImpl(maybeSync: boolean, callback?: (...args: any[]) => void) {
    if (this.#closed === true) {
      throw new ERR_DIR_CLOSED();
    }

    if (callback === undefined) {
      return this.#readPromisified();
    }

    validateFunction(callback, "callback");

    if (this.#operationQueue !== null) {
      ArrayPrototypePush(this.#operationQueue, () => {
        this.#readImpl(maybeSync, callback);
      });
      return;
    }

    try {
      if (this.#processHandlerQueue()) {
        const dirent = this.#shiftEntry();
        if (maybeSync) {
          lazyProcess().default.nextTick(callback, null, dirent);
        } else {
          callback(null, dirent);
        }
        return;
      }
    } catch (error) {
      return callback(error);
    }

    this.#operationQueue = [];
    PromisePrototypeThen(
      readHandle(this.#handle!, this.#options.bufferSize),
      (result) => {
        this.#drainOperationQueue();
        if (result === null) {
          return callback(null, null);
        }
        let dirent;
        try {
          this.#processReadResult(this.#dirPath, result);
          dirent = this.#shiftEntry();
        } catch (error) {
          return callback(error);
        }
        callback(null, dirent);
      },
      (err) => {
        this.#drainOperationQueue();
        callback(err);
      },
    );
  }

  // Node clears the queue in a `nextTick` scheduled before the callback runs,
  // and its `nextTick` queue drains before the promise reactions that resume an
  // `await dir.read()`. Here the read settles in a promise reaction, so the
  // queue is cleared at once; the operations that queued behind the read still
  // run on the next tick.
  #drainOperationQueue() {
    const queue = this.#operationQueue;
    this.#operationQueue = null;
    if (queue !== null && queue.length > 0) {
      lazyProcess().default.nextTick(() => {
        for (const op of new SafeArrayIterator(queue)) op();
      });
    }
  }

  readSync(): Dirent | null {
    if (this.#closed === true) {
      throw new ERR_DIR_CLOSED();
    }

    if (this.#operationQueue !== null) {
      throw new ERR_DIR_CONCURRENT_OPERATION();
    }

    if (this.#processHandlerQueue()) {
      return this.#shiftEntry();
    }

    const result = readHandleSync(this.#handle!, this.#options.bufferSize);
    if (result === null) {
      return null;
    }
    this.#processReadResult(this.#dirPath, result);
    return this.#shiftEntry();
  }

  // deno-lint-ignore no-explicit-any
  close(callback?: (...args: any[]) => void): Promise<void> | void {
    if (callback === undefined) {
      if (this.#closed === true) {
        return PromiseReject(new ERR_DIR_CLOSED());
      }
      return new Promise((resolve, reject) => {
        this.close((err) => err ? reject(err) : resolve());
      });
    }

    validateFunction(callback, "callback");

    if (this.#closed === true) {
      lazyProcess().default.nextTick(callback, new ERR_DIR_CLOSED());
      return;
    }

    if (this.#operationQueue !== null) {
      ArrayPrototypePush(this.#operationQueue, () => {
        this.close(callback);
      });
      return;
    }

    this.#release();
    lazyProcess().default.nextTick(callback, null);
  }

  closeSync() {
    if (this.#closed === true) {
      throw new ERR_DIR_CLOSED();
    }

    if (this.#operationQueue !== null) {
      throw new ERR_DIR_CONCURRENT_OPERATION();
    }

    this.#release();
  }

  #release() {
    this.#handlerQueue = [];
    this.#bufferedEntries = [];
    this.#handle = null;
    this.#closed = true;
  }

  async *entries(): AsyncIterableIterator<Dirent> {
    try {
      while (true) {
        const dirent = await this.#readPromisified();
        if (dirent === null) {
          break;
        }
        yield dirent;
      }
    } finally {
      if (!this.#closed) {
        await this.close();
      }
    }
  }

  [SymbolDispose]() {
    if (this.#closed) return;
    this.closeSync();
  }

  [SymbolAsyncDispose](): Promise<void> {
    if (this.#closed) return PromiseResolve();
    return this.close() as Promise<void>;
  }
}

ObjectDefineProperties(Dir.prototype, {
  [SymbolAsyncIterator]: {
    __proto__: null,
    enumerable: false,
    writable: true,
    configurable: true,
    value: Dir.prototype.entries,
  },
});
