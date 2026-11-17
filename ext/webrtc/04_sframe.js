// Copyright 2018-2026 the Deno authors. MIT license.

// SFrame (RFC 9605) for WebRTC Encoded Transform: `SFrameEncryptorStream` /
// `SFrameDecryptorStream` (transform streams over BufferSources or encoded
// frames), `RTCRtpSFrameEncryptor` / `RTCRtpSFrameDecryptor` (a sender's /
// receiver's `transform`), and `SFrameTransformErrorEvent`.
//
// The cipher suites are RFC 9605's: AES-128-CTR with HMAC-SHA-256 and an
// 80-, 64- or 32-bit tag, AES-128-GCM and AES-256-GCM. The AES-256-CTR /
// HMAC-SHA-512 suites (draft-barnes-sframe-iana-256) are refused with
// NotSupportedError, as is per-packet SFrame. Keys are base keys from which
// each KID's key and salt are derived with HKDF; frames go through Web
// Crypto, one at a time and in order.

/// <reference path="../../core/internal.d.ts" />

(function () {
const { core, internals, primordials } = __bootstrap;
const {
  ArrayBuffer,
  ArrayBufferIsView,
  ArrayPrototypeIncludes,
  BigInt,
  MapPrototypeDelete,
  MapPrototypeGet,
  MapPrototypeSet,
  Number,
  ObjectPrototypeIsPrototypeOf,
  PromisePrototypeThen,
  PromiseReject,
  PromiseResolve,
  RangeError,
  SafeMap,
  Symbol,
  TypeError,
  TypedArrayPrototypeGetBuffer,
  TypedArrayPrototypeGetByteLength,
  TypedArrayPrototypeGetByteOffset,
  TypedArrayPrototypeSet,
  TypedArrayPrototypeSlice,
  TypedArrayPrototypeSubarray,
  Uint8Array,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript(
  "ext:deno_web/01_dom_exception.js",
);
const { defineEventHandler, Event, EventTarget, setIsTrusted } = core
  .loadExtScript("ext:deno_web/02_event.js");
const { TransformStream } = core.loadExtScript("ext:deno_web/06_streams.js");
const cryptoMod = core.loadExtScript("ext:deno_crypto/00_crypto.js");
const transform = core.loadExtScript(
  "ext:deno_webrtc/03_encoded_transform.js",
);

const subtle = () => cryptoMod.crypto.subtle;

// RFC 9605 Table 1, with each suite's IANA value.
const SUITES = {
  __proto__: null,
  AES_128_CTR_HMAC_SHA256_80: { id: 1, hash: "SHA-256", nka: 16, nk: 48, nt: 10 },
  AES_128_CTR_HMAC_SHA256_64: { id: 2, hash: "SHA-256", nka: 16, nk: 48, nt: 8 },
  AES_128_CTR_HMAC_SHA256_32: { id: 3, hash: "SHA-256", nka: 16, nk: 48, nt: 4 },
  AES_128_GCM_SHA256_128: { id: 4, hash: "SHA-256", nka: 0, nk: 16, nt: 16 },
  AES_256_GCM_SHA512_128: { id: 5, hash: "SHA-512", nka: 0, nk: 32, nt: 16 },
};
const ALL_SUITES = [
  "AES_128_CTR_HMAC_SHA256_80",
  "AES_128_CTR_HMAC_SHA256_64",
  "AES_128_CTR_HMAC_SHA256_32",
  "AES_128_GCM_SHA256_128",
  "AES_256_GCM_SHA512_128",
  "AES_256_CTR_HMAC_SHA512_80",
  "AES_256_CTR_HMAC_SHA512_64",
  "AES_256_CTR_HMAC_SHA512_32",
];
const NN = 12;
const MAX_KID = (1n << 64n) - 1n;

function suiteOf(options, prefix) {
  if (options === null || typeof options !== "object") {
    throw new TypeError(`${prefix}: options must be a dictionary`);
  }
  if (options.cipherSuite === undefined) {
    throw new TypeError(`${prefix}: 'cipherSuite' is required`);
  }
  const name = webidl.converters.DOMString(options.cipherSuite, prefix, "cipherSuite");
  if (!ArrayPrototypeIncludes(ALL_SUITES, name)) {
    throw new TypeError(`${prefix}: '${name}' is not a valid SFrameCipherSuite`);
  }
  const suite = SUITES[name];
  if (suite === undefined) {
    throw new DOMException(`${prefix}: ${name} is not supported`, "NotSupportedError");
  }
  return suite;
}

// CryptoKeyID: an [EnforceRange] unsigned long long, or a bigint in 0..2^64-1.
function keyIdOf(value, prefix) {
  if (typeof value === "bigint") {
    if (value < 0n || value > MAX_KID) {
      throw new RangeError(`${prefix}: the key ID is out of range`);
    }
    return value;
  }
  const n = webidl.converters["unsigned long long"](value, prefix, "keyId", {
    enforceRange: true,
  });
  return BigInt(n);
}

function cryptoKeyOf(value, prefix) {
  if (!ObjectPrototypeIsPrototypeOf(cryptoMod.CryptoKey.prototype, value)) {
    throw new TypeError(`${prefix}: the key is not a CryptoKey`);
  }
  return value;
}

// --- RFC 9605 -----------------------------------------------------------------

function bigEndian(value, length) {
  const out = new Uint8Array(length);
  let v = BigInt(value);
  for (let i = length - 1; i >= 0; --i) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}

// The fewest bytes that hold `value` (at least one).
function compactLength(value) {
  let n = 1;
  for (let v = value >> 8n; v > 0n; v >>= 8n) n++;
  return n;
}

function concat(...parts) {
  let length = 0;
  for (let i = 0; i < parts.length; ++i) {
    length += TypedArrayPrototypeGetByteLength(parts[i]);
  }
  const out = new Uint8Array(length);
  let offset = 0;
  for (let i = 0; i < parts.length; ++i) {
    TypedArrayPrototypeSet(out, parts[i], offset);
    offset += TypedArrayPrototypeGetByteLength(parts[i]);
  }
  return out;
}

// RFC 9605 4.3.
function encodeHeader(kid, ctr) {
  let config = 0;
  const parts = [];
  if (kid < 8n) {
    config |= Number(kid) << 4;
  } else {
    const len = compactLength(kid);
    config |= 0x80 | ((len - 1) << 4);
    parts[parts.length] = bigEndian(kid, len);
  }
  if (ctr < 8n) {
    config |= Number(ctr);
  } else {
    const len = compactLength(ctr);
    config |= 0x08 | (len - 1);
    parts[parts.length] = bigEndian(ctr, len);
  }
  return concat(new Uint8Array([config]), ...parts);
}

function readBigEndian(bytes, offset, length) {
  let v = 0n;
  for (let i = 0; i < length; ++i) v = (v << 8n) | BigInt(bytes[offset + i]);
  return v;
}

// { kid, ctr, length } of the header `bytes` start with, or null.
function decodeHeader(bytes) {
  if (TypedArrayPrototypeGetByteLength(bytes) < 1) return null;
  const config = bytes[0];
  let offset = 1;
  let kid = BigInt((config >> 4) & 7);
  if (config & 0x80) {
    const len = Number(kid) + 1;
    if (TypedArrayPrototypeGetByteLength(bytes) < offset + len) return null;
    kid = readBigEndian(bytes, offset, len);
    offset += len;
  }
  let ctr = BigInt(config & 7);
  if (config & 0x08) {
    const len = Number(ctr) + 1;
    if (TypedArrayPrototypeGetByteLength(bytes) < offset + len) return null;
    ctr = readBigEndian(bytes, offset, len);
    offset += len;
  }
  return { kid, ctr, length: offset };
}

// RFC 9605 4.4.2: a KID's key and salt from its base key, as Web Crypto
// keys ready for the suite's AEAD.
async function deriveContext(suite, baseKey, kid) {
  const s = subtle();
  const tail = concat(bigEndian(kid, 8), bigEndian(suite.id, 2));
  const derive = (label, bytes) =>
    s.deriveBits(
      {
        name: "HKDF",
        hash: suite.hash,
        salt: new Uint8Array(0),
        info: concat(core.encode(label), tail),
      },
      baseKey,
      bytes * 8,
    );
  const key = new Uint8Array(await derive("SFrame 1.0 Secret key ", suite.nk));
  const salt = new Uint8Array(await derive("SFrame 1.0 Secret salt ", NN));
  if (suite.nka === 0) {
    return {
      suite,
      salt,
      aes: await s.importKey("raw", key, "AES-GCM", false, [
        "encrypt",
        "decrypt",
      ]),
      hmac: null,
    };
  }
  return {
    suite,
    salt,
    aes: await s.importKey(
      "raw",
      TypedArrayPrototypeSubarray(key, 0, suite.nka),
      "AES-CTR",
      false,
      ["encrypt", "decrypt"],
    ),
    hmac: await s.importKey(
      "raw",
      TypedArrayPrototypeSubarray(key, suite.nka),
      { name: "HMAC", hash: suite.hash },
      false,
      ["sign"],
    ),
  };
}

function nonceOf(ctx, ctr) {
  const nonce = bigEndian(ctr, NN);
  for (let i = 0; i < NN; ++i) nonce[i] ^= ctx.salt[i];
  return nonce;
}

// RFC 9605 4.5.1's tag.
async function ctrTag(ctx, nonce, aad, ct) {
  const nt = ctx.suite.nt;
  const data = concat(
    bigEndian(TypedArrayPrototypeGetByteLength(aad), 8),
    bigEndian(TypedArrayPrototypeGetByteLength(ct), 8),
    bigEndian(nt, 8),
    nonce,
    aad,
    ct,
  );
  const mac = new Uint8Array(await subtle().sign("HMAC", ctx.hmac, data));
  return TypedArrayPrototypeSubarray(mac, 0, nt);
}

// RFC 9605 4.4.3: header + ciphertext (+ tag).
async function encrypt(ctx, kid, ctr, metadata, plaintext) {
  const header = encodeHeader(kid, ctr);
  const nonce = nonceOf(ctx, ctr);
  const aad = concat(header, metadata);
  if (ctx.hmac === null) {
    const ct = new Uint8Array(
      await subtle().encrypt(
        { name: "AES-GCM", iv: nonce, additionalData: aad, tagLength: 128 },
        ctx.aes,
        plaintext,
      ),
    );
    return concat(header, ct);
  }
  const ct = new Uint8Array(
    await subtle().encrypt(
      { name: "AES-CTR", counter: concat(nonce, new Uint8Array(4)), length: 32 },
      ctx.aes,
      plaintext,
    ),
  );
  return concat(header, ct, await ctrTag(ctx, nonce, aad, ct));
}

class SFrameError extends Error {
  constructor(errorType, kid = null) {
    super(`SFrame ${errorType} error`);
    this.errorType = errorType;
    this.kid = kid;
  }
}
const SFrameErrorPrototype = SFrameError.prototype;

// RFC 9605 4.4.4: the plaintext, or an SFrameError.
async function decrypt(lookup, metadata, data) {
  const header = decodeHeader(data);
  if (header === null) throw new SFrameError("syntax");
  const ctx = lookup(header.kid);
  if (ctx === undefined) throw new SFrameError("keyID", header.kid);
  const headerBytes = TypedArrayPrototypeSubarray(data, 0, header.length);
  const body = TypedArrayPrototypeSubarray(data, header.length);
  const nonce = nonceOf(ctx, header.ctr);
  const aad = concat(headerBytes, metadata);
  const nt = ctx.suite.nt;
  if (TypedArrayPrototypeGetByteLength(body) < nt) {
    throw new SFrameError("syntax", header.kid);
  }
  if (ctx.hmac === null) {
    try {
      return new Uint8Array(
        await subtle().decrypt(
          { name: "AES-GCM", iv: nonce, additionalData: aad, tagLength: 128 },
          ctx.aes,
          body,
        ),
      );
    } catch {
      throw new SFrameError("authentication", header.kid);
    }
  }
  const split = TypedArrayPrototypeGetByteLength(body) - nt;
  const ct = TypedArrayPrototypeSubarray(body, 0, split);
  const tag = TypedArrayPrototypeSubarray(body, split);
  const expected = await ctrTag(ctx, nonce, aad, ct);
  let diff = 0;
  for (let i = 0; i < nt; ++i) diff |= tag[i] ^ expected[i];
  if (diff !== 0) throw new SFrameError("authentication", header.kid);
  return new Uint8Array(
    await subtle().decrypt(
      { name: "AES-CTR", counter: concat(nonce, new Uint8Array(4)), length: 32 },
      ctx.aes,
      ct,
    ),
  );
}

// For tests/unit/webrtc_media_test.ts: RFC 9605 Appendix C.3's vectors.
internals.sframeVector = async (suiteName, baseKey, kid, ctr, metadata, pt) => {
  const ctx = await deriveContext(SUITES[suiteName], baseKey, kid);
  const ct = await encrypt(ctx, kid, ctr, metadata, pt);
  const back = await decrypt(() => ctx, metadata, ct);
  return { ct, pt: back };
};

// --- the encryptor and decryptor state --------------------------------------

const _sframe = Symbol("[[sframe]]");
const EMPTY = new Uint8Array(0);

function bytesOf(chunk) {
  if (ArrayBufferIsView(chunk)) {
    return new Uint8Array(
      TypedArrayPrototypeGetBuffer(chunk),
      TypedArrayPrototypeGetByteOffset(chunk),
      TypedArrayPrototypeGetByteLength(chunk),
    );
  }
  return new Uint8Array(chunk);
}

function isBufferSource(chunk) {
  return ArrayBufferIsView(chunk) || core.isAnyArrayBuffer(chunk);
}

// Frames are taken one at a time, in order.
function serialize(state, fn) {
  const next = PromisePrototypeThen(state.queue, fn, fn);
  state.queue = PromisePrototypeThen(next, () => {}, () => {});
  return next;
}

function createEncryptor(suite) {
  return {
    suite,
    kid: null,
    ctx: null,
    counter: 0n,
    queue: PromiseResolve(),
  };
}

function setEncryptionKey(state, key, keyId) {
  return serialize(state, async () => {
    state.ctx = await deriveContext(state.suite, key, keyId);
    state.kid = keyId;
  });
}

// The encrypted bytes of a frame, or null without a key.
function encryptBytes(state, bytes) {
  return serialize(state, async () => {
    if (state.ctx === null) return null;
    const ctr = state.counter;
    state.counter = ctr + 1n;
    return await encrypt(state.ctx, state.kid, ctr, EMPTY, bytes);
  });
}

function createDecryptor(suite, target) {
  return { suite, keys: new SafeMap(), target, queue: PromiseResolve() };
}

function addDecryptionKey(state, key, keyId) {
  return serialize(state, async () => {
    MapPrototypeSet(state.keys, keyId, await deriveContext(state.suite, key, keyId));
  });
}

function removeDecryptionKey(state, keyId) {
  return serialize(state, () => {
    MapPrototypeDelete(state.keys, keyId);
  });
}

// The decrypted bytes, or null after an `error` event on `state.target`.
function decryptBytes(state, bytes, frame) {
  return serialize(state, async () => {
    try {
      return await decrypt(
        (kid) => MapPrototypeGet(state.keys, kid),
        EMPTY,
        bytes,
      );
    } catch (error) {
      if (!ObjectPrototypeIsPrototypeOf(SFrameErrorPrototype, error)) {
        throw error;
      }
      const event = new SFrameTransformErrorEvent("error", {
        errorType: error.errorType,
        keyID: error.kid,
        frame,
      });
      setIsTrusted(event, true);
      state.target.dispatchEvent(event);
      return null;
    }
  });
}

// A chunk in, the same kind of chunk out: an ArrayBuffer for a
// BufferSource, the frame with its data replaced for an encoded frame.
function transformChunk(chunk, controller, run) {
  if (transform.isFrame(chunk)) {
    return PromisePrototypeThen(run(bytesOf(chunk.data), chunk), (out) => {
      if (out === null) return;
      chunk.data = TypedArrayPrototypeGetBuffer(TypedArrayPrototypeSlice(out));
      controller.enqueue(chunk);
    });
  }
  if (!isBufferSource(chunk)) {
    throw new TypeError(
      "SFrame: a chunk must be a BufferSource or an RTCEncoded{Video,Audio}Frame",
    );
  }
  return PromisePrototypeThen(run(bytesOf(chunk), chunk), (out) => {
    if (out === null) {
      // A frame that didn't decrypt: an empty buffer keeps the order.
      controller.enqueue(new ArrayBuffer(0));
      return;
    }
    controller.enqueue(TypedArrayPrototypeGetBuffer(TypedArrayPrototypeSlice(out)));
  });
}

// --- SFrameTransformErrorEvent -----------------------------------------------

const ERROR_TYPES = ["authentication", "keyID", "syntax"];

class SFrameTransformErrorEvent extends Event {
  #errorType;
  #keyID;
  #frame;

  constructor(type, eventInitDict) {
    const prefix = "Failed to construct 'SFrameTransformErrorEvent'";
    webidl.requiredArguments(arguments.length, 2, prefix);
    type = webidl.converters.DOMString(type, prefix, "Argument 1");
    if (eventInitDict === null || typeof eventInitDict !== "object") {
      throw new TypeError(`${prefix}: Argument 2 must be a dictionary`);
    }
    if (eventInitDict.errorType === undefined) {
      throw new TypeError(`${prefix}: 'errorType' is required`);
    }
    if (!("frame" in eventInitDict)) {
      throw new TypeError(`${prefix}: 'frame' is required`);
    }
    const errorType = webidl.converters.DOMString(eventInitDict.errorType);
    if (!ArrayPrototypeIncludes(ERROR_TYPES, errorType)) {
      throw new TypeError(`${prefix}: '${errorType}' is not a valid error type`);
    }
    super(type, eventInitDict);
    this.#errorType = errorType;
    const keyID = eventInitDict.keyID;
    this.#keyID = keyID === undefined || keyID === null
      ? null
      : keyIdOf(keyID, prefix);
    this.#frame = eventInitDict.frame;
  }

  get errorType() {
    if (!(#errorType in this)) throw new TypeError("Illegal invocation");
    return this.#errorType;
  }
  get keyID() {
    if (!(#keyID in this)) throw new TypeError("Illegal invocation");
    // A key ID that fits a Number is one; a larger one stays a bigint.
    const k = this.#keyID;
    return k !== null && k <= 9007199254740991n ? Number(k) : k;
  }
  get frame() {
    if (!(#frame in this)) throw new TypeError("Illegal invocation");
    return this.#frame;
  }
}
webidl.configureInterface(SFrameTransformErrorEvent);

// --- the streams -------------------------------------------------------------

class SFrameEncryptorStream {
  constructor(options) {
    const prefix = "Failed to construct 'SFrameEncryptorStream'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    const state = createEncryptor(suiteOf(options, prefix));
    this[_sframe] = state;
    state.stream = new TransformStream({
      transform: (chunk, controller) =>
        transformChunk(chunk, controller, (bytes) => encryptBytes(state, bytes)),
    });
    this[webidl.brand] = webidl.brand;
  }

  get readable() {
    webidl.assertBranded(this, SFrameEncryptorStreamPrototype);
    return this[_sframe].stream.readable;
  }
  get writable() {
    webidl.assertBranded(this, SFrameEncryptorStreamPrototype);
    return this[_sframe].stream.writable;
  }

  setEncryptionKey(key, keyId) {
    const prefix = "Failed to execute 'setEncryptionKey' on 'SFrameEncryptorStream'";
    try {
      webidl.assertBranded(this, SFrameEncryptorStreamPrototype);
      webidl.requiredArguments(arguments.length, 2, prefix);
      return setEncryptionKey(
        this[_sframe],
        cryptoKeyOf(key, prefix),
        keyIdOf(keyId, prefix),
      );
    } catch (error) {
      return PromiseReject(error);
    }
  }
}
webidl.configureInterface(SFrameEncryptorStream);
const SFrameEncryptorStreamPrototype = SFrameEncryptorStream.prototype;

function decryptorMethods(Class, name) {
  const prototype = Class.prototype;
  Class.prototype.addDecryptionKey = {
    addDecryptionKey(key, keyId) {
      const prefix = `Failed to execute 'addDecryptionKey' on '${name}'`;
      try {
        webidl.assertBranded(this, prototype);
        webidl.requiredArguments(arguments.length, 2, prefix);
        return addDecryptionKey(
          this[_sframe],
          cryptoKeyOf(key, prefix),
          keyIdOf(keyId, prefix),
        );
      } catch (error) {
        return PromiseReject(error);
      }
    },
  }.addDecryptionKey;
  Class.prototype.removeDecryptionKey = {
    removeDecryptionKey(keyId) {
      const prefix = `Failed to execute 'removeDecryptionKey' on '${name}'`;
      try {
        webidl.assertBranded(this, prototype);
        webidl.requiredArguments(arguments.length, 1, prefix);
        return removeDecryptionKey(this[_sframe], keyIdOf(keyId, prefix));
      } catch (error) {
        return PromiseReject(error);
      }
    },
  }.removeDecryptionKey;
  defineEventHandler(prototype, "error");
}

class SFrameDecryptorStream extends EventTarget {
  constructor(options) {
    const prefix = "Failed to construct 'SFrameDecryptorStream'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    const suite = suiteOf(options, prefix);
    super();
    const state = createDecryptor(suite, this);
    this[_sframe] = state;
    state.stream = new TransformStream({
      transform: (chunk, controller) =>
        transformChunk(
          chunk,
          controller,
          (bytes, frame) => decryptBytes(state, bytes, frame),
        ),
    });
    this[webidl.brand] = webidl.brand;
  }

  get readable() {
    webidl.assertBranded(this, SFrameDecryptorStreamPrototype);
    return this[_sframe].stream.readable;
  }
  get writable() {
    webidl.assertBranded(this, SFrameDecryptorStreamPrototype);
    return this[_sframe].stream.writable;
  }
}
decryptorMethods(SFrameDecryptorStream, "SFrameDecryptorStream");
webidl.configureInterface(SFrameDecryptorStream);
const SFrameDecryptorStreamPrototype = SFrameDecryptorStream.prototype;

// --- RTCRtpSFrameEncryptor / RTCRtpSFrameDecryptor ---------------------------

class RTCRtpSFrameEncryptor {
  constructor(options) {
    const prefix = "Failed to construct 'RTCRtpSFrameEncryptor'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    const suite = suiteOf(options, prefix);
    const type = options.type === undefined
      ? "per-frame"
      : webidl.converters.DOMString(options.type, prefix, "type");
    if (type !== "per-frame" && type !== "per-packet") {
      throw new TypeError(`${prefix}: '${type}' is not a valid SFrameType`);
    }
    if (type === "per-packet") {
      throw new DOMException(
        `${prefix}: per-packet SFrame is not supported`,
        "NotSupportedError",
      );
    }
    this[_sframe] = createEncryptor(suite);
    transform.initRtpTransform(this);
    this[webidl.brand] = webidl.brand;
  }

  setEncryptionKey(key, keyId) {
    const prefix = "Failed to execute 'setEncryptionKey' on 'RTCRtpSFrameEncryptor'";
    try {
      webidl.assertBranded(this, RTCRtpSFrameEncryptorPrototype);
      webidl.requiredArguments(arguments.length, 2, prefix);
      return setEncryptionKey(
        this[_sframe],
        cryptoKeyOf(key, prefix),
        keyIdOf(keyId, prefix),
      );
    } catch (error) {
      return PromiseReject(error);
    }
  }
}
webidl.configureInterface(RTCRtpSFrameEncryptor);
const RTCRtpSFrameEncryptorPrototype = RTCRtpSFrameEncryptor.prototype;

class RTCRtpSFrameDecryptor extends EventTarget {
  constructor(options) {
    const prefix = "Failed to construct 'RTCRtpSFrameDecryptor'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    const suite = suiteOf(options, prefix);
    super();
    this[_sframe] = createDecryptor(suite, this);
    transform.initRtpTransform(this);
    this[webidl.brand] = webidl.brand;
  }
}
decryptorMethods(RTCRtpSFrameDecryptor, "RTCRtpSFrameDecryptor");
webidl.configureInterface(RTCRtpSFrameDecryptor);
const RTCRtpSFrameDecryptorPrototype = RTCRtpSFrameDecryptor.prototype;

// 03_encoded_transform.js: a sender's frame through its encryptor, or a
// receiver's through its decryptor. Resolves to the bytes to send (null to
// drop the frame).
function processRtpFrame(owner, side, bytes) {
  const state = owner[_sframe];
  return side === "sender"
    ? encryptBytes(state, bytes)
    : decryptBytes(state, bytes, null);
}

function isSFrameTransform(value, side) {
  return side === "sender"
    ? ObjectPrototypeIsPrototypeOf(RTCRtpSFrameEncryptorPrototype, value)
    : ObjectPrototypeIsPrototypeOf(RTCRtpSFrameDecryptorPrototype, value);
}

return {
  RTCRtpSFrameDecryptor,
  RTCRtpSFrameEncryptor,
  SFrameDecryptorStream,
  SFrameEncryptorStream,
  SFrameTransformErrorEvent,
  isSFrameTransform,
  processRtpFrame,
};
})();
