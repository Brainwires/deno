// Copyright 2018-2026 the Deno authors. MIT license.

// node:sea for `deno compile` programs: a compiled program is Deno's single
// executable application, and its assets are the files it carries (the main
// module's directory and everything `--include` added). An asset's key is
// its path relative to the main module's directory, or an absolute path
// inside the compiled program. Only embedded files are ever read.

(function () {
const { core, primordials } = __bootstrap;
const {
  ArrayBufferPrototypeSlice,
  StringPrototypeLastIndexOf,
  StringPrototypeSlice,
  TypedArrayPrototypeGetBuffer,
  TypedArrayPrototypeGetByteLength,
  TypedArrayPrototypeGetByteOffset,
} = primordials;
const { op_main_module, op_node_sea_get_asset } = core.ops;
const { isWindows } = core.loadExtScript("ext:deno_node/_util/os.ts");
const { NodeError } = core.loadExtScript("ext:deno_node/internal/errors.ts");
const { validateString } = core.loadExtScript(
  "ext:deno_node/internal/validators.mjs",
);
const { Blob } = core.loadExtScript("ext:deno_web/09_file.js");
const { TextDecoder } = core.loadExtScript("ext:deno_web/08_text_encoding.js");
const { pathFromURL } = core.loadExtScript("ext:deno_web/00_infra.js");

class ERR_NOT_IN_SINGLE_EXECUTABLE_APPLICATION extends NodeError {
  constructor() {
    super(
      "ERR_NOT_IN_SINGLE_EXECUTABLE_APPLICATION",
      "This feature is only available in single executable applications",
    );
  }
}

class ERR_SINGLE_EXECUTABLE_APPLICATION_ASSET_NOT_FOUND extends NodeError {
  constructor(key) {
    super(
      "ERR_SINGLE_EXECUTABLE_APPLICATION_ASSET_NOT_FOUND",
      `Cannot find asset ${key} for the single executable application`,
    );
  }
}

function isSea() {
  // No internal flag carries this: the runtime sets it on `Deno.build` only
  // (child_process.ts and process.ts read it there too).
  return Deno.build.standalone === true;
}

function isAbsolutePath(path) {
  return path[0] === "/" || path[0] === "\\" ||
    (path.length > 2 && path[1] === ":" &&
      (path[2] === "\\" || path[2] === "/"));
}

function mainModuleDir() {
  const main = pathFromURL(op_main_module());
  const cut = StringPrototypeLastIndexOf(main, isWindows ? "\\" : "/");
  return cut === -1 ? main : StringPrototypeSlice(main, 0, cut);
}

function readAsset(key) {
  if (!isSea()) {
    throw new ERR_NOT_IN_SINGLE_EXECUTABLE_APPLICATION();
  }
  validateString(key, "key");
  // The runtime normalizes the path and reads only embedded files.
  const path = isAbsolutePath(key) ? key : `${mainModuleDir()}/${key}`;
  let bytes;
  try {
    bytes = op_node_sea_get_asset(path);
  } catch (err) {
    if (err?.name === "NotFound") {
      throw new ERR_SINGLE_EXECUTABLE_APPLICATION_ASSET_NOT_FOUND(key);
    }
    throw err;
  }
  // An ArrayBuffer holding exactly the asset's bytes, the caller's own.
  const offset = TypedArrayPrototypeGetByteOffset(bytes);
  return ArrayBufferPrototypeSlice(
    TypedArrayPrototypeGetBuffer(bytes),
    offset,
    offset + TypedArrayPrototypeGetByteLength(bytes),
  );
}

/** The asset's bytes, in an ArrayBuffer of its own (or decoded text). */
function getAsset(key, encoding) {
  if (encoding !== undefined) {
    validateString(encoding, "encoding");
  }
  const buffer = readAsset(key);
  if (encoding === undefined) {
    return buffer;
  }
  return new TextDecoder(encoding).decode(buffer);
}

/** The asset's bytes. Each call returns a fresh ArrayBuffer. */
function getRawAsset(key) {
  return readAsset(key);
}

/** The asset as a Blob (`options` as the Blob constructor takes). */
function getAssetAsBlob(key, options) {
  return new Blob([readAsset(key)], options);
}

return {
  isSea,
  getAsset,
  getRawAsset,
  getAssetAsBlob,
};
})();
