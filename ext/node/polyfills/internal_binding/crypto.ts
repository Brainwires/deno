// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Joyent, Inc. and Node.js contributors. All rights reserved. MIT license.

(function () {
const { core, primordials } = __bootstrap;
const { timingSafeEqual } = core.loadExtScript(
  "ext:deno_node/internal_binding/_timingSafeEqual.ts",
);
const { NodeRangeError } = core.loadExtScript(
  "ext:deno_node/internal/errors.ts",
);
const { op_node_argon2_async, op_node_argon2_sync } = core.ops;
const { isAnyArrayBuffer, isDataView } = core;

const {
  DataViewPrototypeGetBuffer,
  DataViewPrototypeGetByteLength,
  DataViewPrototypeGetByteOffset,
  Error,
  PromisePrototypeThen,
  TypedArrayPrototypeGetBuffer,
  TypedArrayPrototypeGetByteLength,
  TypedArrayPrototypeGetByteOffset,
  Uint8Array,
} = primordials;

function getFipsCrypto(): boolean {
  return false;
}

function setFipsCrypto(_fips: boolean) {
  throw new Error("FIPS mode is not supported in Deno.");
}

// Node's CryptoJobMode.
const kCryptoJobAsync = 0;
const kCryptoJobSync = 1;

// ncrypto's Argon2Type.
const kTypeArgon2d = 0;
const kTypeArgon2i = 1;
const kTypeArgon2id = 2;

// ArrayBufferOrViewContents::CheckSizeInt32() in Node's binding.
const kMaxInt32 = 2 ** 31 - 1;

// The bytes of an ArrayBuffer or ArrayBufferView, as a Uint8Array over the
// same memory.
function bytesOf(
  value: ArrayBufferLike | ArrayBufferView,
  name: string,
): Uint8Array {
  let bytes;
  if (isAnyArrayBuffer(value)) {
    bytes = new Uint8Array(value);
  } else if (isDataView(value)) {
    bytes = new Uint8Array(
      DataViewPrototypeGetBuffer(value),
      DataViewPrototypeGetByteOffset(value),
      DataViewPrototypeGetByteLength(value),
    );
  } else {
    bytes = new Uint8Array(
      TypedArrayPrototypeGetBuffer(value),
      TypedArrayPrototypeGetByteOffset(value),
      TypedArrayPrototypeGetByteLength(value),
    );
  }
  if (TypedArrayPrototypeGetByteLength(bytes) > kMaxInt32) {
    throw new NodeRangeError("ERR_OUT_OF_RANGE", `${name} is too large`);
  }
  return bytes;
}

// Node's `internalBinding('crypto').Argon2Job`: no validation beyond the
// binding's own (lib/internal/crypto/argon2.js validates first), and the
// KDF's refusals come back as `Argon2 derivation failed`.
class Argon2Job {
  ondone: ((err: Error | undefined, result?: ArrayBuffer) => void) | undefined;
  #mode: number;
  #args: [
    Uint8Array,
    Uint8Array,
    number,
    number,
    number,
    number,
    Uint8Array,
    Uint8Array,
    number,
  ];

  constructor(
    mode: number,
    pass: ArrayBufferLike | ArrayBufferView,
    salt: ArrayBufferLike | ArrayBufferView,
    lanes: number,
    keylen: number,
    memcost: number,
    iter: number,
    secret: ArrayBufferLike | ArrayBufferView,
    ad: ArrayBufferLike | ArrayBufferView,
    type: number,
  ) {
    this.ondone = undefined;
    this.#mode = mode;
    this.#args = [
      bytesOf(pass, "pass"),
      bytesOf(salt, "salt"),
      lanes >>> 0,
      keylen >>> 0,
      memcost >>> 0,
      iter >>> 0,
      bytesOf(secret, "secret"),
      bytesOf(ad, "ad"),
      type >>> 0,
    ];
  }

  run() {
    const args = this.#args;
    if (this.#mode === kCryptoJobSync) {
      let result;
      try {
        result = op_node_argon2_sync(
          args[0],
          args[1],
          args[2],
          args[3],
          args[4],
          args[5],
          args[6],
          args[7],
          args[8],
        );
      } catch (err) {
        return [err, undefined];
      }
      return [undefined, TypedArrayPrototypeGetBuffer(result)];
    }
    // The async op copies its inputs before it leaves for the blocking
    // pool, as Node's async job does.
    PromisePrototypeThen(
      op_node_argon2_async(
        args[0],
        args[1],
        args[2],
        args[3],
        args[4],
        args[5],
        args[6],
        args[7],
        args[8],
      ),
      (result: Uint8Array) => {
        this.ondone?.(undefined, TypedArrayPrototypeGetBuffer(result));
      },
      (err: Error) => {
        this.ondone?.(err, undefined);
      },
    );
    return undefined;
  }
}

return {
  timingSafeEqual,
  getFipsCrypto,
  setFipsCrypto,
  Argon2Job,
  kCryptoJobAsync,
  kCryptoJobSync,
  kTypeArgon2d,
  kTypeArgon2i,
  kTypeArgon2id,
};
})();
