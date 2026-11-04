// Copyright 2018-2026 the Deno authors. MIT license.

// deno-lint-ignore-file no-explicit-any

import { core } from "ext:core/mod.js";
import type { WriteFileOptions } from "ext:deno_node/_fs/_fs_common.ts";
import type { Encodings } from "ext:deno_node/_utils.ts";
const { promisify } = core.loadExtScript("ext:deno_node/internal/util.mjs");
const constants = core.loadExtScript("ext:deno_node/_fs/_fs_constants.ts");
import { copyFilePromise } from "ext:deno_node/_fs/_fs_copy.ts";
const { cpPromise } = core.loadExtScript("ext:deno_node/_fs/_fs_cp.ts");
import { lutimesPromise } from "ext:deno_node/_fs/_fs_lutimes.ts";
import { readdirPromise } from "ext:deno_node/_fs/_fs_readdir.ts";
const { lstatPromise } = core.loadExtScript("ext:deno_node/_fs/_fs_lstat.ts");
const lazyFs = core.createLazyLoader("node:fs");
import { globPromise } from "ext:deno_node/_fs/_fs_glob.ts";
import {
  getValidatedPathToString,
  validateStringAfterArrayBufferView,
  vfsState,
} from "ext:deno_node/internal/fs/utils.mjs";
const { isIterable } = core.loadExtScript(
  "ext:deno_node/internal/streams/utils.js",
);
import type { Buffer } from "node:buffer";
import Dir from "ext:deno_node/_fs/_fs_dir.ts";
import { FileHandle } from "ext:deno_node/internal/fs/handle.ts";
import { primordials } from "ext:core/mod.js";
const { parseFileMode } = core.loadExtScript(
  "ext:deno_node/internal/validators.mjs",
);
import { op_node_lchmod } from "ext:core/ops";
const { isMacOS } = core.loadExtScript("ext:deno_node/_util/os.ts");
const { ERR_METHOD_NOT_IMPLEMENTED, aggregateTwoErrors } = core.loadExtScript(
  "ext:deno_node/internal/errors.ts",
);
const lazyPath = core.createLazyLoader("node:path");
const lazyProcess = core.createLazyLoader("node:process");

const {
  ArrayBufferIsView,
  ErrorCaptureStackTrace,
  ErrorPrototype,
  ObjectDefineProperty,
  ObjectFreeze,
  ObjectPrototypeIsPrototypeOf,
  Promise,
  PromisePrototypeThen,
  PromiseReject,
  PromiseResolve,
  ReflectApply,
  SafeArrayIterator,
  SymbolAsyncDispose,
} = primordials;

// Node's lib/internal/fs/promises.js `handleErrorFromBinding`: a rejection's
// error gets its stack captured here, inside a promise reaction the calling
// `fs.promises` function awaits, so the trace has no internal frames and
// lists the caller's own async frames (`at async Object.access`, then the
// user code that awaited it) instead of a frame inside the polyfill.
function handleErrorFromBinding(err: unknown): never {
  if (
    err !== null && typeof err === "object" &&
    ObjectPrototypeIsPrototypeOf(ErrorPrototype, err)
  ) {
    ErrorCaptureStackTrace(err, handleErrorFromBinding);
  }
  throw err;
}

// Promisified fs.X wrappers MUST NOT be built at module body. handle.ts /
// internal/fs/promises.ts are loaded during the initial `fs.promises`
// access, and calling `lazyFs()` here re-enters `node:fs`'s evaluating body
// (its `export const promises = mod.promises` line re-triggers `get
// promises` on fs.ts, whose lazyInternalPromises() then hits a TDZ on the
// in-flight `default` binding). Build wrappers lazily on first call.
const _promisifyCache: Record<string, any> = { __proto__: null } as any;
// `arity` is the max positional arg count the underlying fs callback method
// accepts (excluding the callback). Extra args are dropped so idiomatic
// patterns like `paths.map(fs.promises.unlink)` -- which Array#map invokes as
// `unlink(elem, index, array)` -- don't trip the promisify wrapper's appended
// callback (the wrapper would otherwise call `fs.unlink(path, index, array,
// cb)` and `fs.unlink` reads the second positional as the callback). Node's
// own `fs.promises.*` wrappers don't go through `util.promisify` and so don't
// have this issue.
function lazyPromisifyFs(name: string, arity: number): any {
  // An `async function` (not an arrow) named after the `fs.promises` method,
  // as Node's are: argument errors reject instead of throwing, and an
  // error's async stack trace names the method.
  const wrapper = async function (...args: any[]) {
    let fn = _promisifyCache[name];
    if (fn === undefined) {
      fn = promisify((lazyFs() as any)[name]);
      _promisifyCache[name] = fn;
    }
    if (args.length > arity) args.length = arity;
    return await PromisePrototypeThen(
      fn(...new SafeArrayIterator(args)),
      undefined,
      handleErrorFromBinding,
    );
  };
  ObjectDefineProperty(wrapper, "name", { __proto__: null, value: name });
  return wrapper;
}

// Mirrors Node's lib/internal/fs/promises.js handleFdClose(): run the file op,
// then close the FileHandle. Looks up `fh.close` lazily so tests that
// monkey-patch the prototype/instance close still take effect.
//   op ok, close ok       -> resolve(result)
//   op ok, close throws   -> throw closeError
//   op throws, close ok   -> throw opError
//   op throws, close throws -> throw AggregateError([opError, closeError])
async function handleFdClose<T>(
  fileOpPromise: Promise<T>,
  closeFunc: () => Promise<void>,
): Promise<T> {
  let result: T;
  let opError: unknown;
  let opFailed = false;
  try {
    result = await fileOpPromise;
  } catch (err) {
    opError = err;
    opFailed = true;
  }
  try {
    await closeFunc();
  } catch (closeError) {
    if (opFailed) {
      // Mirrors Node's aggregateTwoErrors(): preserves opError.code on the
      // AggregateError so callers asserting err.code see the op's code.
      throw aggregateTwoErrors(closeError, opError);
    }
    throw closeError;
  }
  if (opFailed) {
    throw opError;
  }
  return result!;
}

// -- access --

const accessPromise = lazyPromisifyFs("access", 2) as (
  path: string | Buffer | URL,
  mode?: number,
) => Promise<void>;

// -- appendFile --

// Delegates to writeFilePromise with an "a" flag, mirroring Node's
// lib/internal/fs/promises.js appendFile(). Per Node semantics, when given a
// FileHandle the existing flag stays in effect.
function appendFilePromise(
  path: string | number | URL | FileHandle,
  data: string | Uint8Array,
  options?: Encodings | WriteFileOptions,
): Promise<void> {
  let opts: WriteFileOptions;
  if (typeof options === "string") {
    opts = { encoding: options };
  } else if (options == null || typeof options !== "object") {
    opts = {};
  } else {
    opts = { ...options };
  }
  opts.flag = opts.flag || "a";
  return writeFilePromise(path, data, opts);
}

// -- chmod --

const chmodPromise = lazyPromisifyFs("chmod", 2) as (
  path: string | Buffer | URL,
  mode: string | number,
) => Promise<void>;

// -- chown --

const chownPromise = lazyPromisifyFs("chown", 3) as (
  path: string | Buffer | URL,
  uid: number,
  gid: number,
) => Promise<void>;

const lchmodPromise: (
  path: string | Buffer | URL,
  mode: number,
) => Promise<void> = !isMacOS
  ? () => PromiseReject(new ERR_METHOD_NOT_IMPLEMENTED("lchmod()"))
  : async (path: string | Buffer | URL, mode: number) => {
    path = getValidatedPathToString(path);
    mode = parseFileMode(mode, "mode");
    return await op_node_lchmod(path, mode);
  };

const lchownPromise = lazyPromisifyFs("lchown", 3) as (
  path: string | Buffer | URL,
  uid: number,
  gid: number,
) => Promise<void>;

const linkPromise = lazyPromisifyFs("link", 2) as (
  existingPath: string | Buffer | URL,
  newPath: string | Buffer | URL,
) => Promise<void>;

const unlinkPromise = lazyPromisifyFs("unlink", 1) as (
  path: string | Buffer | URL,
) => Promise<void>;

const renamePromise = lazyPromisifyFs("rename", 2) as (
  oldPath: string | Buffer | URL,
  newPath: string | Buffer | URL,
) => Promise<void>;

// -- rm --

type rmOptions = {
  force?: boolean;
  maxRetries?: number;
  recursive?: boolean;
  retryDelay?: number;
};

const rmPromise = lazyPromisifyFs("rm", 2) as (
  path: string | URL,
  options?: rmOptions,
) => Promise<void>;

// -- rmdir --

type rmdirOptions = {
  maxRetries?: number;
  recursive?: boolean;
  retryDelay?: number;
};

const rmdirPromise = lazyPromisifyFs("rmdir", 2) as (
  path: string | Buffer | URL,
  options?: rmdirOptions,
) => Promise<void>;

type MkdirOptions =
  | { recursive?: boolean; mode?: number | undefined }
  | number
  | boolean;

const mkdirPromise = lazyPromisifyFs("mkdir", 2) as (
  path: string | URL,
  options?: MkdirOptions,
) => Promise<string | undefined>;

const mkdtempPromise = lazyPromisifyFs("mkdtemp", 2) as (
  prefix: string | Buffer | Uint8Array | URL,
  options?: { encoding: string } | string,
) => Promise<string>;

// Mirrors Node's lib/internal/fs/promises.js mkdtempDisposable(): create the
// temp dir, then return an object with .path, .remove(), and Symbol.asyncDispose
// that recursively removes the directory. Capture cwd at creation time so a
// later process.chdir() doesn't break removal.
async function mkdtempDisposablePromise(
  prefix: string | Buffer | Uint8Array | URL,
  options?: { encoding: string } | string,
) {
  const cwd = lazyProcess().default.cwd();
  const path = await mkdtempPromise(prefix, options);
  const fullPath = lazyPath().resolve(cwd, path);
  // `force: true` makes the second remove() a no-op when the dir is already
  // gone (Node's rimraf-based implementation treats ENOENT as success); other
  // errors (EACCES, EPERM, ...) still propagate.
  const remove = async () => {
    await rmPromise(fullPath, {
      force: true,
      maxRetries: 0,
      recursive: true,
      retryDelay: 0,
    });
  };
  return {
    __proto__: null,
    path,
    remove,
    async [SymbolAsyncDispose]() {
      await remove();
    },
  };
}

type OpenFlags =
  | "a"
  | "ax"
  | "a+"
  | "ax+"
  | "as"
  | "as+"
  | "r"
  | "r+"
  | "rs"
  | "rs+"
  | "w"
  | "wx"
  | "w+"
  | "wx+"
  | number
  | string;

function openPromise(
  path: string | Buffer | URL,
  flags: OpenFlags = "r",
  mode = 0o666,
): Promise<FileHandle> {
  return new Promise((resolve, reject) => {
    lazyFs().open(path, flags, mode, (err, fd) => {
      if (err) reject(err);
      else resolve(new FileHandle(fd as number));
    });
  });
}

type OpendirOptions = {
  encoding?: string;
  bufferSize?: number;
};

const opendirPromise = lazyPromisifyFs("opendir", 2) as (
  path: string | Buffer | URL,
  options?: OpendirOptions,
) => Promise<Dir>;

// -- symlink --

const symlinkPromise = lazyPromisifyFs("symlink", 3) as (
  target: string | Buffer | URL,
  path: string | Buffer | URL,
  type?: string,
) => Promise<void>;

// -- truncate --

// Mirrors Node's lib/internal/fs/promises.js truncate(): open the path as a
// FileHandle, delegate to its truncate method, then close via handleFdClose
// so callers that monkey-patch FileHandle still observe the fd access and
// AggregateError-on-double-failure semantics that Node tests rely on.
async function truncatePromise(
  path: string | URL,
  len?: number,
): Promise<void> {
  const fh = await openPromise(path, "r+");
  return handleFdClose(fh.truncate(len), () => fh.close());
}

// -- utimes --

const utimesPromise = lazyPromisifyFs("utimes", 3) as (
  path: string | URL,
  atime: number | string | Date,
  mtime: number | string | Date,
) => Promise<void>;

// -- writeFile --

// Low-level callback writeFile, used when we already have an fd/FileHandle
// (i.e. avoid recursing back through writeFilePromise via FileHandle.writeFile).
const rawWriteFilePromise = lazyPromisifyFs("writeFile", 3) as (
  pathOrRid: string | number | URL | FileHandle,
  data:
    | string
    | DataView
    | NodeJS.TypedArray
    | AsyncIterable<NodeJS.TypedArray | string>,
  options?: Encodings | WriteFileOptions,
) => Promise<void>;

// Mirrors Node's lib/internal/fs/promises.js writeFile(): when given a path,
// open a FileHandle and delegate via handleFdClose so the close error
// semantics are observable; when given an fd/FileHandle, write directly.
function writeFilePromise(
  pathOrRid: string | number | URL | FileHandle,
  data:
    | string
    | DataView
    | NodeJS.TypedArray
    | AsyncIterable<NodeJS.TypedArray | string>,
  options?: Encodings | WriteFileOptions,
): Promise<void> {
  if (
    typeof pathOrRid === "number" ||
    ObjectPrototypeIsPrototypeOf(FileHandle.prototype, pathOrRid)
  ) {
    return rawWriteFilePromise(pathOrRid, data, options);
  }
  const opts: WriteFileOptions = typeof options === "string"
    ? { encoding: options }
    : (options ?? {});
  const flag = opts.flag ?? "w";
  const mode = opts.mode ?? 0o666;
  return (async () => {
    // Node validates `data` before it opens (and so creates) the file.
    if (
      !ArrayBufferIsView(data) &&
      (typeof data === "string" || !isIterable(data))
    ) {
      validateStringAfterArrayBufferView(data, "data");
    }
    // Match the existing path-based behavior: surface the same `DOMException`
    // that `signal.throwIfAborted()` produces (the fd-based fallback would
    // throw Deno's `AbortError` instead). Inside the async IIFE so the throw
    // becomes a promise rejection, not a sync throw.
    if (opts.signal?.aborted) opts.signal.throwIfAborted();
    const fh = await openPromise(
      pathOrRid as string | Buffer | URL,
      flag,
      mode,
    );
    return handleFdClose(fh.writeFile(data, opts), () => fh.close());
  })();
}

// -- realpath --

const realpathPromise = lazyPromisifyFs("realpath", 2) as (
  path: string | Buffer,
  options?: string | { encoding?: string },
) => Promise<string | Buffer>;

// -- stat --

const statPromise = lazyPromisifyFs("stat", 2) as (
  path: string | Buffer | URL,
  options?: { bigint?: boolean },
) => Promise<unknown>;

// -- statfs --

const statfsPromise = lazyPromisifyFs("statfs", 2) as (
  path: string | Buffer | URL,
  options?: { bigint?: boolean },
) => Promise<unknown>;

// -- readFile / readlink --

// Low-level callback readFile, used when we already have an fd/FileHandle
// (i.e. avoid recursing back through readFilePromise via FileHandle.readFile).
const rawReadFilePromise = lazyPromisifyFs("readFile", 2);

// Mirrors Node's lib/internal/fs/promises.js readFile(): when given a path,
// open a FileHandle and delegate via handleFdClose so the close error
// semantics are observable; when given an fd/FileHandle, read directly.
function readFilePromise(
  path: string | number | URL | FileHandle,
  options?: Encodings | {
    encoding?: Encodings;
    flag?: string;
    signal?: AbortSignal;
  },
): Promise<string | Buffer> {
  if (
    typeof path === "number" ||
    ObjectPrototypeIsPrototypeOf(FileHandle.prototype, path)
  ) {
    return rawReadFilePromise(path, options);
  }
  const opts: { encoding?: Encodings; flag?: string; signal?: AbortSignal } =
    typeof options === "string" ? { encoding: options } : (options ?? {});
  const flag = opts.flag ?? "r";
  return (async () => {
    // Match the existing path-based behavior: surface the same `DOMException`
    // that `signal.throwIfAborted()` produces (the fd-based fallback would
    // throw Deno's `AbortError` instead). Inside the async IIFE so the throw
    // becomes a promise rejection, not a sync throw.
    if (opts.signal?.aborted) opts.signal.throwIfAborted();
    const fh = await openPromise(path as string | Buffer | URL, flag);
    return handleFdClose(fh.readFile(opts), () => fh.close()) as Promise<
      string | Buffer
    >;
  })();
}

const readlinkPromise = lazyPromisifyFs("readlink", 2) as (
  path: string | Buffer | URL,
  opt?: { encoding?: string | null },
) => Promise<string | Uint8Array>;

// -- promises object --

// node:vfs: as in Node's lib/internal/fs/promises.js, each of these asks the
// mounted virtual file systems (`vfsState.handlers`, internal/vfs/setup.js)
// first. A handler returns `undefined` for a path or descriptor that is not
// virtual, and the call then goes to the real file system as before.
type VfsHandlers = Record<string, (...args: any[]) => any>;

// A handler call that throws rejects the returned promise, as Node's async
// functions do. The real file system's path calls the implementation
// directly, so its promise (and when it settles) is unchanged.
function vfsCall(call: () => unknown): unknown {
  try {
    return call();
  } catch (err) {
    return PromiseReject(err);
  }
}

const kDefaultStatOptions = ObjectFreeze({ __proto__: null, bigint: false });
const kDefaultStatThrowOptions = ObjectFreeze({
  __proto__: null,
  bigint: false,
  throwIfNoEntry: true,
});

// A handler that resolves with nothing (or nothing the caller sees). `call`
// gets the handlers and the call's arguments.
function vfsVoidFirst<R>(
  impl: (...args: any[]) => Promise<R>,
  call: (h: VfsHandlers, a: any[]) => Promise<unknown> | undefined,
): (...args: any[]) => Promise<R | void> {
  return function (this: unknown, ...args: any[]) {
    const h = vfsState.handlers;
    if (h !== null) {
      const promise = vfsCall(() => call(h, args));
      if (promise !== undefined) {
        return PromisePrototypeThen(PromiseResolve(promise), () => {});
      }
    }
    // The receiver is kept, so an error's async stack still names the
    // method (`at async Object.access`), as Node's does.
    return ReflectApply(impl, this, args);
  };
}

// A handler whose result (or its promise's) is the call's result.
function vfsResultFirst<R>(
  impl: (...args: any[]) => Promise<R>,
  call: (h: VfsHandlers, a: any[]) => unknown,
): (...args: any[]) => Promise<R> {
  return function (this: unknown, ...args: any[]) {
    const h = vfsState.handlers;
    if (h !== null) {
      const result = vfsCall(() => call(h, args));
      if (result !== undefined) return PromiseResolve(result) as Promise<R>;
    }
    // The receiver is kept, so an error's async stack still names the
    // method (`at async Object.access`), as Node's does.
    return ReflectApply(impl, this, args);
  };
}

function named<T extends (...args: any[]) => any>(name: string, fn: T): T {
  ObjectDefineProperty(fn, "name", {
    __proto__: null,
    value: name,
    configurable: true,
  });
  return fn;
}

const vfsAccess = named(
  "access",
  vfsVoidFirst(accessPromise, (h, a) => h.access(a[0], a[1])),
);
const vfsCopyFile = named(
  "copyFile",
  vfsVoidFirst(
    copyFilePromise,
    (h, a) => h.copyFile(a[0], a[1], a[2]),
  ),
);
const vfsOpen = named(
  "open",
  vfsResultFirst(
    openPromise,
    (h, a) => h.promisesOpen(a[0], a[1], a[2]),
  ),
);
const vfsRename = named(
  "rename",
  vfsVoidFirst(
    renamePromise,
    (h, a) => h.rename(a[0], a[1]),
  ),
);
const vfsTruncate = named(
  "truncate",
  vfsVoidFirst(truncatePromise, (h, a) => h.truncate(a[0], a[1] ?? 0)),
);
const vfsRm = named(
  "rm",
  vfsVoidFirst(rmPromise, (h, a) => h.rm(a[0], a[1])),
);
const vfsRmdir = named(
  "rmdir",
  vfsVoidFirst(rmdirPromise, (h, a) => h.rmdir(a[0])),
);
const vfsMkdir = named(
  "mkdir",
  function (this: unknown, path: string | Buffer | URL, options?: unknown) {
    const h = vfsState.handlers;
    if (h !== null) {
      const promise = vfsCall(() => h.mkdir(path, options));
      if (promise !== undefined) {
        return PromisePrototypeThen(
          PromiseResolve(promise),
          (r: { result: unknown }) => r.result,
        );
      }
    }
    return ReflectApply(mkdirPromise, this, [path, options]);
  },
);
const vfsReaddir = named(
  "readdir",
  vfsResultFirst(
    readdirPromise,
    (h, a) => h.readdir(a[0], a[1]),
  ),
);
const vfsReadlink = named(
  "readlink",
  vfsResultFirst(
    readlinkPromise,
    (h, a) => h.readlink(a[0], a[1]),
  ),
);
const vfsSymlink = named(
  "symlink",
  vfsVoidFirst(
    symlinkPromise,
    (h, a) => h.symlink(a[0], a[1], a[2]),
  ),
);
const vfsLstat = named(
  "lstat",
  vfsResultFirst(
    lstatPromise,
    (h, a) => h.lstat(a[0], a[1] ?? kDefaultStatOptions),
  ),
);
const vfsStat = named(
  "stat",
  vfsResultFirst(
    statPromise,
    (h, a) => h.stat(a[0], a[1] ?? kDefaultStatThrowOptions),
  ),
);
const vfsStatfs = named(
  "statfs",
  vfsResultFirst(
    statfsPromise,
    (h, a) => h.statfs(a[0], a[1] ?? kDefaultStatOptions),
  ),
);
const vfsLink = named(
  "link",
  vfsVoidFirst(
    linkPromise,
    (h, a) => h.link(a[0], a[1]),
  ),
);
const vfsUnlink = named(
  "unlink",
  vfsVoidFirst(unlinkPromise, (h, a) => h.unlink(a[0])),
);
const vfsChmod = named(
  "chmod",
  vfsVoidFirst(chmodPromise, (h, a) => h.chmod(a[0], a[1])),
);
const vfsLchmod = lchmodPromise === undefined ? undefined : named(
  "lchmod",
  vfsVoidFirst(lchmodPromise, (h, a) => h.lchmod(a[0], a[1])),
);
const vfsLchown = named(
  "lchown",
  vfsVoidFirst(
    lchownPromise,
    (h, a) => h.lchown(a[0], a[1], a[2]),
  ),
);
const vfsChown = named(
  "chown",
  vfsVoidFirst(chownPromise, (h, a) => h.chown(a[0], a[1], a[2])),
);
const vfsUtimes = named(
  "utimes",
  vfsVoidFirst(
    utimesPromise,
    (h, a) => h.utimes(a[0], a[1], a[2]),
  ),
);
const vfsLutimes = named(
  "lutimes",
  vfsVoidFirst(
    lutimesPromise,
    (h, a) => h.lutimes(a[0], a[1], a[2]),
  ),
);
const vfsRealpath = named(
  "realpath",
  vfsResultFirst(
    realpathPromise,
    (h, a) => h.realpath(a[0], a[1]),
  ),
);
const vfsMkdtemp = named(
  "mkdtemp",
  vfsResultFirst(
    mkdtempPromise,
    (h, a) => h.mkdtemp(a[0], a[1]),
  ),
);
const vfsWriteFile = named(
  "writeFile",
  vfsVoidFirst(
    writeFilePromise,
    (h, a) => h.writeFile(a[0], a[1], a[2]),
  ),
);
const vfsAppendFile = named(
  "appendFile",
  vfsVoidFirst(
    appendFilePromise,
    (h, a) => h.appendFile(a[0], a[1], a[2]),
  ),
);
const vfsReadFile = named(
  "readFile",
  vfsResultFirst(
    readFilePromise,
    (h, a) => h.readFile(a[0], a[1]),
  ),
);

const promises = {
  access: vfsAccess,
  constants,
  copyFile: vfsCopyFile,
  cp: cpPromise,
  glob: globPromise,
  open: vfsOpen,
  opendir: opendirPromise,
  rename: vfsRename,
  truncate: vfsTruncate,
  rm: vfsRm,
  rmdir: vfsRmdir,
  mkdir: vfsMkdir,
  readdir: vfsReaddir,
  readlink: vfsReadlink,
  symlink: vfsSymlink,
  lstat: vfsLstat,
  stat: vfsStat,
  statfs: vfsStatfs,
  link: vfsLink,
  unlink: vfsUnlink,
  chmod: vfsChmod,
  lchmod: vfsLchmod,
  lchown: vfsLchown,
  chown: vfsChown,
  utimes: vfsUtimes,
  lutimes: vfsLutimes,
  realpath: vfsRealpath,
  mkdtemp: vfsMkdtemp,
  mkdtempDisposable: mkdtempDisposablePromise,
  writeFile: vfsWriteFile,
  appendFile: vfsAppendFile,
  readFile: vfsReadFile,
  watch: ((...args: any[]) => {
    const h = vfsState.handlers;
    if (h !== null) {
      const result = h.promisesWatch?.(args[0], args[1] ?? {});
      if (result !== undefined) return result;
    }
    return (lazyFs() as any).watchPromise(...new SafeArrayIterator(args));
  }) as any,
};

export default promises;

export { constants, FileHandle, mkdirPromise, opendirPromise };
