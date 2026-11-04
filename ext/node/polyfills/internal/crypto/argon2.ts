// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Joyent, Inc. and Node.js contributors. All rights reserved. MIT license.

// deno-lint-ignore-file no-explicit-any

// Node's lib/internal/crypto/argon2.js (Node 24.7+): the same validation,
// errors and defaults, over the same `Argon2Job` binding shape.
// `getArrayBufferOrView` answers a TypedArray for every input (an ArrayBuffer
// or a DataView becomes a Uint8Array over the same memory), so its byteLength
// is the TypedArray getter's.

(function () {
const { core, primordials } = __bootstrap;
const {
  FunctionPrototypeCall,
  MathPow,
  TypedArrayPrototypeGetByteLength,
  Uint8Array,
} = primordials;
const { Buffer } = core.loadExtScript("ext:deno_node/internal/buffer.mjs");
const {
  Argon2Job,
  kCryptoJobAsync,
  kCryptoJobSync,
  kTypeArgon2d,
  kTypeArgon2i,
  kTypeArgon2id,
} = core.loadExtScript("ext:deno_node/internal_binding/crypto.ts");
const { getArrayBufferOrView } = core.loadExtScript(
  "ext:deno_node/internal/crypto/keys.ts",
);
const {
  validateFunction,
  validateInteger,
  validateObject,
  validateOneOf,
  validateString,
  validateUint32,
} = core.loadExtScript("ext:deno_node/internal/validators.mjs");

function argon2(algorithm: any, parameters: any, callback: any) {
  parameters = check(algorithm, parameters);

  validateFunction(callback, "callback");

  const job = new Argon2Job(
    kCryptoJobAsync,
    parameters.message,
    parameters.nonce,
    parameters.parallelism,
    parameters.tagLength,
    parameters.memory,
    parameters.passes,
    parameters.secret,
    parameters.associatedData,
    parameters.type,
  );

  job.ondone = (error: any, result: any) => {
    if (error !== undefined) {
      return FunctionPrototypeCall(callback, job, error);
    }
    const buf = Buffer.from(result);
    return FunctionPrototypeCall(callback, job, null, buf);
  };

  job.run();
}

function argon2Sync(algorithm: any, parameters: any) {
  parameters = check(algorithm, parameters);

  const job = new Argon2Job(
    kCryptoJobSync,
    parameters.message,
    parameters.nonce,
    parameters.parallelism,
    parameters.tagLength,
    parameters.memory,
    parameters.passes,
    parameters.secret,
    parameters.associatedData,
    parameters.type,
  );

  const { 0: err, 1: result } = job.run();

  if (err !== undefined) {
    throw err;
  }

  return Buffer.from(result);
}

function check(algorithm: any, parameters: any) {
  validateString(algorithm, "algorithm");
  validateOneOf(algorithm, "algorithm", ["argon2d", "argon2i", "argon2id"]);

  let type;
  switch (algorithm) {
    case "argon2d":
      type = kTypeArgon2d;
      break;
    case "argon2i":
      type = kTypeArgon2i;
      break;
    default:
      type = kTypeArgon2id;
      break;
  }

  validateObject(parameters, "parameters");

  const { parallelism, tagLength, memory, passes } = parameters;
  const MAX_POSITIVE_UINT_32 = MathPow(2, 32) - 1;

  const message = getArrayBufferOrView(
    parameters.message,
    "parameters.message",
  );
  validateInteger(
    TypedArrayPrototypeGetByteLength(message),
    "parameters.message.byteLength",
    0,
    MAX_POSITIVE_UINT_32,
  );

  const nonce = getArrayBufferOrView(parameters.nonce, "parameters.nonce");
  validateInteger(
    TypedArrayPrototypeGetByteLength(nonce),
    "parameters.nonce.byteLength",
    8,
    MAX_POSITIVE_UINT_32,
  );

  validateInteger(
    parallelism,
    "parameters.parallelism",
    1,
    MathPow(2, 24) - 1,
  );
  validateInteger(tagLength, "parameters.tagLength", 4, MAX_POSITIVE_UINT_32);
  // At least 8 KiB per lane (RFC 9106) and at most 2^32 - 1 KiB: the bound
  // that keeps a hostile `memory` from asking for more than that.
  validateInteger(
    memory,
    "parameters.memory",
    8 * parallelism,
    MAX_POSITIVE_UINT_32,
  );
  validateUint32(passes, "parameters.passes", true);

  let secret;
  if (parameters.secret === undefined) {
    secret = new Uint8Array(0);
  } else {
    secret = getArrayBufferOrView(parameters.secret);
    validateInteger(
      TypedArrayPrototypeGetByteLength(secret),
      "parameters.secret.byteLength",
      0,
      MAX_POSITIVE_UINT_32,
    );
  }

  let associatedData;
  if (parameters.associatedData === undefined) {
    associatedData = new Uint8Array(0);
  } else {
    associatedData = getArrayBufferOrView(parameters.associatedData);
    validateInteger(
      TypedArrayPrototypeGetByteLength(associatedData),
      "parameters.associatedData.byteLength",
      0,
      MAX_POSITIVE_UINT_32,
    );
  }

  return {
    message,
    nonce,
    secret,
    associatedData,
    tagLength,
    passes,
    parallelism,
    memory,
    type,
  };
}

return {
  argon2,
  argon2Sync,
  default: {
    argon2,
    argon2Sync,
  },
};
})();
