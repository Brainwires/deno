// Copyright 2018-2026 the Deno authors. MIT license.

import { core, primordials } from "ext:core/mod.js";
import type { CallbackWithError } from "ext:deno_node/_fs/_fs_common.ts";
const { makeCallback } = core.loadExtScript(
  "ext:deno_node/_fs/_fs_common.ts",
);
import type { Buffer } from "node:buffer";
import {
  getValidatedPathToString,
  getValidMode,
} from "ext:deno_node/internal/fs/utils.mjs";

const {
  Error,
  ObjectPrototypeIsPrototypeOf,
  PromisePrototypeThen,
} = primordials;
const { fs } = core.loadExtScript(
  "ext:deno_node/internal_binding/constants.ts",
);
const { codeMap } = core.loadExtScript("ext:deno_node/internal_binding/uv.ts");
const { promisify } = core.loadExtScript("ext:deno_node/internal/util.mjs");
const { denoErrorToNodeError } = core.loadExtScript(
  "ext:deno_node/internal/errors.ts",
);

// Node's copyfile errors name the source as `path` and the destination as
// `dest` (`ENOENT: no such file or directory, copyfile '<src>' -> '<dest>'`).
function copyFileError(err: unknown, src: string, dest: string) {
  if (!ObjectPrototypeIsPrototypeOf(Error.prototype, err)) return err;
  return denoErrorToNodeError(err as Error, {
    syscall: "copyfile",
    path: src,
    dest,
  });
}

function copyFileExistsError(src: string, dest: string) {
  // deno-lint-ignore no-explicit-any
  const e: any = new Error(
    `EEXIST: file already exists, copyfile '${src}' -> '${dest}'`,
  );
  e.errno = codeMap.get("EEXIST");
  e.code = "EEXIST";
  e.syscall = "copyfile";
  e.path = src;
  e.dest = dest;
  return e;
}

export function copyFile(
  src: string | Buffer | URL,
  dest: string | Buffer | URL,
  callback: CallbackWithError,
): void;
export function copyFile(
  src: string | Buffer | URL,
  dest: string | Buffer | URL,
  mode: number,
  callback: CallbackWithError,
): void;
export function copyFile(
  src: string | Buffer | URL,
  dest: string | Buffer | URL,
  mode: number | CallbackWithError,
  callback?: CallbackWithError,
) {
  if (typeof mode === "function") {
    callback = mode;
    mode = 0;
  }
  const srcStr = getValidatedPathToString(src, "src");
  const destStr = getValidatedPathToString(dest, "dest");
  const modeNum = getValidMode(mode, "copyFile");
  const cb = makeCallback(callback);

  const failed = (e: unknown) => cb(copyFileError(e, srcStr, destStr));
  if ((modeNum & fs.COPYFILE_EXCL) === fs.COPYFILE_EXCL) {
    PromisePrototypeThen(Deno.lstat(destStr), () => {
      // copyfile(2) reports a missing source before an existing destination.
      PromisePrototypeThen(
        Deno.stat(srcStr),
        () => cb(copyFileExistsError(srcStr, destStr)),
        failed,
      );
    }, (e) => {
      if (ObjectPrototypeIsPrototypeOf(Deno.errors.NotFound.prototype, e)) {
        PromisePrototypeThen(
          Deno.copyFile(srcStr, destStr),
          () => cb(null),
          failed,
        );
      } else {
        failed(e);
      }
    });
  } else {
    PromisePrototypeThen(
      Deno.copyFile(srcStr, destStr),
      () => cb(null),
      failed,
    );
  }
}

export const copyFilePromise = promisify(copyFile) as (
  src: string | Buffer | URL,
  dest: string | Buffer | URL,
  mode?: number,
) => Promise<void>;

export function copyFileSync(
  src: string | Buffer | URL,
  dest: string | Buffer | URL,
  mode?: number,
) {
  const srcStr = getValidatedPathToString(src, "src");
  const destStr = getValidatedPathToString(dest, "dest");
  const modeNum = getValidMode(mode, "copyFile");

  let destExists = false;
  if ((modeNum & fs.COPYFILE_EXCL) === fs.COPYFILE_EXCL) {
    try {
      Deno.lstatSync(destStr);
      destExists = true;
    } catch (e) {
      if (!ObjectPrototypeIsPrototypeOf(Deno.errors.NotFound.prototype, e)) {
        throw copyFileError(e, srcStr, destStr);
      }
    }
  }
  if (destExists) {
    // copyfile(2) reports a missing source before an existing destination.
    try {
      Deno.statSync(srcStr);
    } catch (e) {
      throw copyFileError(e, srcStr, destStr);
    }
    throw copyFileExistsError(srcStr, destStr);
  }
  try {
    Deno.copyFileSync(srcStr, destStr);
  } catch (e) {
    throw copyFileError(e, srcStr, destStr);
  }
}
