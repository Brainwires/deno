// Copyright 2018-2026 the Deno authors. MIT license.

(function () {
const { core, primordials } = __bootstrap;
const { op_node_fill_random } = core.ops;
const {
  Error,
  ErrorPrototype,
  MathMin,
  NumberIsNaN,
  ObjectPrototypeIsPrototypeOf,
} = primordials;

const { Buffer, kMaxLength } = core.loadExtScript(
  "ext:deno_node/internal/buffer.mjs",
);
const {
  emitAfter,
  emitBefore,
  emitDestroy,
  emitInit,
  executionAsyncId,
  newAsyncId,
} = core.loadExtScript("ext:deno_node/internal/async_hooks.ts");
const {
  validateFunction,
  validateNumber,
} = core.loadExtScript("ext:deno_node/internal/validators.mjs");
const { ERR_OUT_OF_RANGE } = core.loadExtScript(
  "ext:deno_node/internal/errors.ts",
);

const lazyProcess = core.createLazyLoader("node:process");

const MAX_RANDOM_VALUES = 65536;
const kMaxInt32 = 2 ** 31 - 1;
const kMaxPossibleLength = MathMin(kMaxLength, kMaxInt32);
const MAX_SIZE = kMaxPossibleLength;

// Mirrors Node's lib/internal/crypto/random.js assertSize() with
// elementSize = 1, offset = 0, length = Infinity.
function assertSize(size: number): number {
  validateNumber(size, "size");

  if (NumberIsNaN(size) || size > kMaxPossibleLength || size < 0) {
    throw new ERR_OUT_OF_RANGE(
      "size",
      `>= 0 && <= ${kMaxPossibleLength}`,
      size,
    );
  }

  return size >>> 0;
}

// One op call fills the whole buffer: the 65,536-byte cap is WebCrypto's
// (`getRandomValues`), not Node's. Under `--seed` the op draws from the same
// seeded RNG as `getRandomValues`, so the byte sequence is unchanged.
function generateRandomBytes(size: number) {
  const bytes = Buffer.allocUnsafeSlow(size);
  op_node_fill_random(bytes);
  return bytes;
}

/**
 * @param size Buffer length, must be equal or greater than zero
 */
function randomBytes(
  size: number,
  cb?: (err: Error | null, buf?: Buffer) => void,
): Buffer | void {
  size = assertSize(size);
  if (cb !== undefined) {
    validateFunction(cb, "callback");
  }
  if (typeof cb === "function") {
    let err: Error | null = null, bytes: Buffer;
    try {
      bytes = generateRandomBytes(size);
    } catch (e) {
      if (ObjectPrototypeIsPrototypeOf(ErrorPrototype, e)) {
        err = e;
      } else {
        err = new Error("[non-error thrown]");
      }
    }

    // Set up async_hooks tracking
    const asyncId = newAsyncId();
    const triggerAsyncId = executionAsyncId();
    const resource = {};
    emitInit(asyncId, "RANDOMBYTESREQUEST", triggerAsyncId, resource);

    const process = lazyProcess().default;
    process.nextTick(() => {
      emitBefore(asyncId, triggerAsyncId, resource);
      try {
        try {
          if (err) {
            cb(err);
          } else {
            cb(null, bytes);
          }
        } catch (callbackErr) {
          // If there's an active domain, emit error to it
          if (process.domain && process.domain.listenerCount("error") > 0) {
            process.domain.emit("error", callbackErr);
          } else {
            // As Node's MakeCallback: the uncaught exception is handled while
            // the request is still the current resource; its `after` is
            // emitted once that is done (process._fatalException).
            throw callbackErr;
          }
        }
        emitAfter(asyncId);
      } finally {
        emitDestroy(asyncId);
      }
    });
  } else {
    return generateRandomBytes(size);
  }
}

return { default: randomBytes, randomBytes, MAX_RANDOM_VALUES, MAX_SIZE };
})();
