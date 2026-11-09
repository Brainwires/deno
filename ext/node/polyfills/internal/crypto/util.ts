// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Joyent, Inc. and Node.js contributors. All rights reserved. MIT license.

(function () {
const { core, primordials } = __bootstrap;
const {
  ArrayPrototypeSlice,
  MapPrototypeGet,
  MapPrototypeSet,
  SafeArrayIterator,
  SafeMap,
} = primordials;
const { notImplemented } = core.loadExtScript("ext:deno_node/_utils.ts");
const { Buffer } = core.loadExtScript("ext:deno_node/internal/buffer.mjs");
const {
  ERR_CRYPTO_INVALID_DIGEST,
  ERR_INVALID_ARG_TYPE,
  hideStackFrames,
} = core.loadExtScript("ext:deno_node/internal/errors.ts");
const {
  isAnyArrayBuffer,
  isArrayBufferView,
} = core.loadExtScript("ext:deno_node/internal/util/types.ts");
const { crypto: constants } = core.loadExtScript(
  "ext:deno_node/internal_binding/constants.ts",
);
const {
  validateInt32,
  validateObject,
} = core.loadExtScript("ext:deno_node/internal/validators.mjs");
const {
  kHandle,
  kKeyObject,
} = core.loadExtScript("ext:deno_node/internal/crypto/constants.ts");

type EllipticCurve = {
  name: string;
  ephemeral: boolean;
  privateKeySize: number;
  publicKeySize: number;
  publicKeySizeCompressed: number;
  sharedSecretSize: number;
};

const ellipticCurves: Array<EllipticCurve> = [
  {
    name: "secp256k1",
    privateKeySize: 32,
    publicKeySize: 65,
    publicKeySizeCompressed: 33,
    sharedSecretSize: 32,
  }, // Weierstrass-class EC used by Bitcoin
  {
    name: "prime256v1",
    privateKeySize: 32,
    publicKeySize: 65,
    publicKeySizeCompressed: 33,
    sharedSecretSize: 32,
  }, // NIST P-256 EC
  {
    name: "secp256r1",
    privateKeySize: 32,
    publicKeySize: 65,
    publicKeySizeCompressed: 33,
    sharedSecretSize: 32,
  }, // NIST P-256 EC (same as above)
  {
    name: "secp384r1",
    privateKeySize: 48,
    publicKeySize: 97,
    publicKeySizeCompressed: 49,
    sharedSecretSize: 48,
  }, // NIST P-384 EC
  {
    name: "secp224r1",
    privateKeySize: 28,
    publicKeySize: 57,
    publicKeySizeCompressed: 29,
    sharedSecretSize: 28,
  }, // NIST P-224 EC
  {
    name: "brainpoolP256r1",
    privateKeySize: 32,
    publicKeySize: 65,
    publicKeySizeCompressed: 33,
    sharedSecretSize: 32,
  }, // RFC 5639 (DENEXT.md item 292)
  {
    name: "brainpoolP384r1",
    privateKeySize: 48,
    publicKeySize: 97,
    publicKeySizeCompressed: 49,
    sharedSecretSize: 48,
  }, // RFC 5639 (DENEXT.md item 292)
];

// OpenSSL NID values and cipher metadata.
// NID values sourced from OpenSSL's include/openssl/obj_mac.h.
interface CipherInfoResult {
  name: string;
  nid: number;
  blockSize: number;
  ivLength: number;
  keyLength: number;
  mode: string;
}

const cipherInfoTable: CipherInfoResult[] = [
  {
    name: "aes-128-ecb",
    nid: 418,
    blockSize: 16,
    ivLength: 0,
    keyLength: 16,
    mode: "ecb",
  },
  {
    name: "aes-128-cbc",
    nid: 419,
    blockSize: 16,
    ivLength: 16,
    keyLength: 16,
    mode: "cbc",
  },
  {
    name: "aes-192-ecb",
    nid: 422,
    blockSize: 16,
    ivLength: 0,
    keyLength: 24,
    mode: "ecb",
  },
  {
    name: "aes-192-cbc",
    nid: 423,
    blockSize: 16,
    ivLength: 16,
    keyLength: 24,
    mode: "cbc",
  },
  {
    name: "aes-256-ecb",
    nid: 426,
    blockSize: 16,
    ivLength: 0,
    keyLength: 32,
    mode: "ecb",
  },
  {
    name: "aes-256-cbc",
    nid: 427,
    blockSize: 16,
    ivLength: 16,
    keyLength: 32,
    mode: "cbc",
  },
  {
    name: "aes-128-gcm",
    nid: 895,
    blockSize: 1,
    ivLength: 12,
    keyLength: 16,
    mode: "gcm",
  },
  {
    name: "aes-192-gcm",
    nid: 898,
    blockSize: 1,
    ivLength: 12,
    keyLength: 24,
    mode: "gcm",
  },
  {
    name: "aes-256-gcm",
    nid: 901,
    blockSize: 1,
    ivLength: 12,
    keyLength: 32,
    mode: "gcm",
  },
  {
    name: "des-ede3-cbc",
    nid: 44,
    blockSize: 8,
    ivLength: 8,
    keyLength: 24,
    mode: "cbc",
  },
  {
    name: "aes-128-ctr",
    nid: 904,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "ctr",
  },
  {
    name: "aes-192-ctr",
    nid: 905,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "ctr",
  },
  {
    name: "aes-256-ctr",
    nid: 906,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "ctr",
  },
  {
    name: "chacha20",
    nid: 1019,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "stream",
  },
  {
    name: "chacha20-poly1305",
    nid: 1018,
    blockSize: 1,
    ivLength: 12,
    keyLength: 32,
    mode: "",
  },
  // AES Key Wrap (RFC 3394): NID_id_aes{128,192,256}_wrap = 788,789,790
  {
    name: "aes128-wrap",
    nid: 788,
    blockSize: 8,
    ivLength: 8,
    keyLength: 16,
    mode: "wrap",
  },
  {
    name: "aes192-wrap",
    nid: 789,
    blockSize: 8,
    ivLength: 8,
    keyLength: 24,
    mode: "wrap",
  },
  {
    name: "aes256-wrap",
    nid: 790,
    blockSize: 8,
    ivLength: 8,
    keyLength: 32,
    mode: "wrap",
  },
  // AES Key Wrap with Padding (RFC 5649): NID 897,900,903 (interleaved with GCM/CCM)
  {
    name: "id-aes128-wrap-pad",
    nid: 897,
    blockSize: 8,
    ivLength: 4,
    keyLength: 16,
    mode: "wrap",
  },
  {
    name: "id-aes192-wrap-pad",
    nid: 900,
    blockSize: 8,
    ivLength: 4,
    keyLength: 24,
    mode: "wrap",
  },
  {
    name: "id-aes256-wrap-pad",
    nid: 903,
    blockSize: 8,
    ivLength: 4,
    keyLength: 32,
    mode: "wrap",
  },
  // DENEXT.md item 292: the ciphers of `cipher_legacy.rs`, as Node 26 reports them
  {
    name: "aes-128-cfb",
    nid: 421,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "cfb",
  },
  {
    name: "aes-128-cfb1",
    nid: 650,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "cfb",
  },
  {
    name: "aes-128-cfb8",
    nid: 653,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "cfb",
  },
  {
    name: "aes-128-ocb",
    nid: 958,
    blockSize: 16,
    ivLength: 12,
    keyLength: 16,
    mode: "ocb",
  },
  {
    name: "aes-128-ofb",
    nid: 420,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "ofb",
  },
  {
    name: "aes-128-xts",
    nid: 913,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "xts",
  },
  {
    name: "aes-192-cfb",
    nid: 425,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "aes-192-cfb1",
    nid: 651,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "aes-192-cfb8",
    nid: 654,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "aes-192-ocb",
    nid: 959,
    blockSize: 16,
    ivLength: 12,
    keyLength: 24,
    mode: "ocb",
  },
  {
    name: "aes-192-ofb",
    nid: 424,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "ofb",
  },
  {
    name: "aes-256-cfb",
    nid: 429,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "cfb",
  },
  {
    name: "aes-256-cfb1",
    nid: 652,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "cfb",
  },
  {
    name: "aes-256-cfb8",
    nid: 655,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "cfb",
  },
  {
    name: "aes-256-ocb",
    nid: 960,
    blockSize: 16,
    ivLength: 12,
    keyLength: 32,
    mode: "ocb",
  },
  {
    name: "aes-256-ofb",
    nid: 428,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "ofb",
  },
  {
    name: "aes-256-xts",
    nid: 914,
    blockSize: 1,
    ivLength: 16,
    keyLength: 64,
    mode: "xts",
  },
  {
    name: "camellia-128-cbc",
    nid: 751,
    blockSize: 16,
    ivLength: 16,
    keyLength: 16,
    mode: "cbc",
  },
  {
    name: "camellia-128-cfb",
    nid: 757,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "cfb",
  },
  {
    name: "camellia-128-cfb1",
    nid: 760,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "cfb",
  },
  {
    name: "camellia-128-cfb8",
    nid: 763,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "cfb",
  },
  {
    name: "camellia-128-ctr",
    nid: 963,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "ctr",
  },
  {
    name: "camellia-128-ecb",
    nid: 754,
    blockSize: 16,
    keyLength: 16,
    mode: "ecb",
  },
  {
    name: "camellia-128-ofb",
    nid: 766,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "ofb",
  },
  {
    name: "camellia-192-cbc",
    nid: 752,
    blockSize: 16,
    ivLength: 16,
    keyLength: 24,
    mode: "cbc",
  },
  {
    name: "camellia-192-cfb",
    nid: 758,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "camellia-192-cfb1",
    nid: 761,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "camellia-192-cfb8",
    nid: 764,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "camellia-192-ctr",
    nid: 967,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "ctr",
  },
  {
    name: "camellia-192-ecb",
    nid: 755,
    blockSize: 16,
    keyLength: 24,
    mode: "ecb",
  },
  {
    name: "camellia-192-ofb",
    nid: 767,
    blockSize: 1,
    ivLength: 16,
    keyLength: 24,
    mode: "ofb",
  },
  {
    name: "camellia-256-cbc",
    nid: 753,
    blockSize: 16,
    ivLength: 16,
    keyLength: 32,
    mode: "cbc",
  },
  {
    name: "camellia-256-cfb",
    nid: 759,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "cfb",
  },
  {
    name: "camellia-256-cfb1",
    nid: 762,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "cfb",
  },
  {
    name: "camellia-256-cfb8",
    nid: 765,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "cfb",
  },
  {
    name: "camellia-256-ctr",
    nid: 971,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "ctr",
  },
  {
    name: "camellia-256-ecb",
    nid: 756,
    blockSize: 16,
    keyLength: 32,
    mode: "ecb",
  },
  {
    name: "camellia-256-ofb",
    nid: 768,
    blockSize: 1,
    ivLength: 16,
    keyLength: 32,
    mode: "ofb",
  },
  {
    name: "des-ede",
    nid: 32,
    blockSize: 8,
    keyLength: 16,
    mode: "ecb",
  },
  {
    name: "des-ede-cbc",
    nid: 43,
    blockSize: 8,
    ivLength: 8,
    keyLength: 16,
    mode: "cbc",
  },
  {
    name: "des-ede-cfb",
    nid: 60,
    blockSize: 1,
    ivLength: 8,
    keyLength: 16,
    mode: "cfb",
  },
  {
    name: "des-ede-ofb",
    nid: 62,
    blockSize: 1,
    ivLength: 8,
    keyLength: 16,
    mode: "ofb",
  },
  {
    name: "des-ede3",
    nid: 33,
    blockSize: 8,
    keyLength: 24,
    mode: "ecb",
  },
  {
    name: "des-ede3-cfb",
    nid: 61,
    blockSize: 1,
    ivLength: 8,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "des-ede3-cfb1",
    nid: 658,
    blockSize: 1,
    ivLength: 8,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "des-ede3-cfb8",
    nid: 659,
    blockSize: 1,
    ivLength: 8,
    keyLength: 24,
    mode: "cfb",
  },
  {
    name: "des-ede3-ofb",
    nid: 63,
    blockSize: 1,
    ivLength: 8,
    keyLength: 24,
    mode: "ofb",
  },
  {
    name: "id-aes128-ccm",
    nid: 896,
    blockSize: 1,
    ivLength: 12,
    keyLength: 16,
    mode: "ccm",
  },
  {
    name: "id-aes128-gcm",
    nid: 895,
    blockSize: 1,
    ivLength: 12,
    keyLength: 16,
    mode: "gcm",
  },
  {
    name: "id-aes192-ccm",
    nid: 899,
    blockSize: 1,
    ivLength: 12,
    keyLength: 24,
    mode: "ccm",
  },
  {
    name: "id-aes192-gcm",
    nid: 898,
    blockSize: 1,
    ivLength: 12,
    keyLength: 24,
    mode: "gcm",
  },
  {
    name: "id-aes256-ccm",
    nid: 902,
    blockSize: 1,
    ivLength: 12,
    keyLength: 32,
    mode: "ccm",
  },
  {
    name: "id-aes256-gcm",
    nid: 901,
    blockSize: 1,
    ivLength: 12,
    keyLength: 32,
    mode: "gcm",
  },
  {
    name: "sm4-cbc",
    nid: 1134,
    blockSize: 16,
    ivLength: 16,
    keyLength: 16,
    mode: "cbc",
  },
  {
    name: "sm4-cfb",
    nid: 1137,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "cfb",
  },
  {
    name: "sm4-ctr",
    nid: 1139,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "ctr",
  },
  {
    name: "sm4-ecb",
    nid: 1133,
    blockSize: 16,
    keyLength: 16,
    mode: "ecb",
  },
  {
    name: "sm4-ofb",
    nid: 1135,
    blockSize: 1,
    ivLength: 16,
    keyLength: 16,
    mode: "ofb",
  },
];

const cipherInfoByName = new SafeMap<string, CipherInfoResult>();
const cipherInfoByNid = new SafeMap<number, CipherInfoResult>();

for (const info of new SafeArrayIterator(cipherInfoTable)) {
  MapPrototypeSet(cipherInfoByName, info.name, info);
  MapPrototypeSet(cipherInfoByNid, info.nid, info);
}

// Aliases
MapPrototypeSet(
  cipherInfoByName,
  "aes128",
  MapPrototypeGet(cipherInfoByName, "aes-128-cbc")!,
);
MapPrototypeSet(
  cipherInfoByName,
  "aes192",
  MapPrototypeGet(cipherInfoByName, "aes-192-cbc")!,
);
MapPrototypeSet(
  cipherInfoByName,
  "aes256",
  MapPrototypeGet(cipherInfoByName, "aes-256-cbc")!,
);
for (
  const { 0: alias, 1: name } of new SafeArrayIterator([
    ["aes-128-ccm", "id-aes128-ccm"],
    ["aes-192-ccm", "id-aes192-ccm"],
    ["aes-192-gcm", "id-aes192-gcm"],
    ["aes-256-ccm", "id-aes256-ccm"],
    ["camellia128", "camellia-128-cbc"],
    ["camellia192", "camellia-192-cbc"],
    ["camellia256", "camellia-256-cbc"],
    ["des-ede-ecb", "des-ede"],
    ["des-ede3-ecb", "des-ede3"],
    ["des3", "des-ede3-cbc"],
    ["sm4", "sm4-cbc"],
    ["sm4-cfb128", "sm4-cfb"],
    ["sm4-ofb128", "sm4-ofb"],
  ])
) {
  MapPrototypeSet(
    cipherInfoByName,
    alias,
    MapPrototypeGet(cipherInfoByName, name)!,
  );
}

// Ciphers actually supported by the runtime (subset of cipherInfoTable);
// the legacy ones come from ext/node_crypto/cipher_legacy.rs (DENEXT.md item
// 292).
// Must be kept in sorted (lexicographic) order - Node.js validates this.
const supportedCiphers = [
  "aes-128-cbc",
  "aes-128-ccm",
  "aes-128-cfb",
  "aes-128-cfb1",
  "aes-128-cfb8",
  "aes-128-ctr",
  "aes-128-ecb",
  "aes-128-gcm",
  "aes-128-ocb",
  "aes-128-ofb",
  "aes-128-xts",
  "aes-192-cbc",
  "aes-192-ccm",
  "aes-192-cfb",
  "aes-192-cfb1",
  "aes-192-cfb8",
  "aes-192-ctr",
  "aes-192-ecb",
  "aes-192-gcm",
  "aes-192-ocb",
  "aes-192-ofb",
  "aes-256-cbc",
  "aes-256-ccm",
  "aes-256-cfb",
  "aes-256-cfb1",
  "aes-256-cfb8",
  "aes-256-ctr",
  "aes-256-ecb",
  "aes-256-gcm",
  "aes-256-ocb",
  "aes-256-ofb",
  "aes-256-xts",
  "aes128",
  "aes128-wrap",
  "aes192",
  "aes192-wrap",
  "aes256",
  "aes256-wrap",
  "camellia-128-cbc",
  "camellia-128-cfb",
  "camellia-128-cfb1",
  "camellia-128-cfb8",
  "camellia-128-ctr",
  "camellia-128-ecb",
  "camellia-128-ofb",
  "camellia-192-cbc",
  "camellia-192-cfb",
  "camellia-192-cfb1",
  "camellia-192-cfb8",
  "camellia-192-ctr",
  "camellia-192-ecb",
  "camellia-192-ofb",
  "camellia-256-cbc",
  "camellia-256-cfb",
  "camellia-256-cfb1",
  "camellia-256-cfb8",
  "camellia-256-ctr",
  "camellia-256-ecb",
  "camellia-256-ofb",
  "camellia128",
  "camellia192",
  "camellia256",
  "chacha20",
  "chacha20-poly1305",
  "des-ede",
  "des-ede-cbc",
  "des-ede-cfb",
  "des-ede-ecb",
  "des-ede-ofb",
  "des-ede3",
  "des-ede3-cbc",
  "des-ede3-cfb",
  "des-ede3-cfb1",
  "des-ede3-cfb8",
  "des-ede3-ecb",
  "des-ede3-ofb",
  "des3",
  "id-aes128-ccm",
  "id-aes128-gcm",
  "id-aes128-wrap-pad",
  "id-aes192-ccm",
  "id-aes192-gcm",
  "id-aes192-wrap-pad",
  "id-aes256-ccm",
  "id-aes256-gcm",
  "id-aes256-wrap-pad",
  "sm4",
  "sm4-cbc",
  "sm4-cfb",
  "sm4-cfb128",
  "sm4-ctr",
  "sm4-ecb",
  "sm4-ofb",
  "sm4-ofb128",
];

function getCiphers(): string[] {
  return ArrayPrototypeSlice(supportedCiphers);
}

const hashBlockSizes: Record<string, number> = {
  md5: 64,
  rmd160: 64,
  ripemd160: 64,
  sha1: 64,
  // Historical OpenSSL alias for sha1, also accepted by the digest ops.
  dss1: 64,
  sha224: 64,
  sha256: 64,
  sha384: 128,
  sha512: 128,
  "sha512-224": 128,
  "sha512-256": 128,
  "sha3-224": 144,
  "sha3-256": 136,
  "sha3-384": 104,
  "sha3-512": 72,
  blake2b512: 128,
  blake2s256: 64,
};

function getHashBlockSize(algorithm: string): number {
  const blockSize = hashBlockSizes[algorithm];
  if (blockSize === undefined) {
    throw new ERR_CRYPTO_INVALID_DIGEST(algorithm);
  }
  return blockSize;
}

function getCipherInfo(
  nameOrNid: string | number,
  options?: { keyLength?: number; ivLength?: number },
) {
  if (typeof nameOrNid !== "string" && typeof nameOrNid !== "number") {
    throw new ERR_INVALID_ARG_TYPE(
      "nameOrNid",
      ["string", "number"],
      nameOrNid,
    );
  }

  if (typeof nameOrNid === "number") {
    validateInt32(nameOrNid, "nameOrNid");
  }

  let keyLength, ivLength;

  if (options !== undefined) {
    validateObject(options, "options");

    ({ keyLength, ivLength } = options);

    if (keyLength !== undefined) {
      validateInt32(keyLength, "options.keyLength");
    }

    if (ivLength !== undefined) {
      validateInt32(ivLength, "options.ivLength");
    }
  }

  const info = typeof nameOrNid === "number"
    ? MapPrototypeGet(cipherInfoByNid, nameOrNid)
    : MapPrototypeGet(cipherInfoByName, nameOrNid);

  if (info === undefined) {
    return undefined;
  }

  if (keyLength !== undefined && info.keyLength !== keyLength) {
    return undefined;
  }

  if (ivLength !== undefined && info.ivLength !== ivLength) {
    return undefined;
  }

  return { ...info };
}

let defaultEncoding = "buffer";

function setDefaultEncoding(val: string) {
  defaultEncoding = val;
}

function getDefaultEncoding(): string {
  return defaultEncoding;
}

// This is here because many functions accepted binary strings without
// any explicit encoding in older versions of node, and we don't want
// to break them unnecessarily.
function toBuf(val: string | Buffer, encoding?: string): Buffer {
  if (typeof val === "string") {
    if (encoding === "buffer") {
      encoding = "utf8";
    }

    return Buffer.from(val, encoding);
  }

  return val;
}

const validateByteSource = hideStackFrames((val, name) => {
  val = toBuf(val);

  if (isAnyArrayBuffer(val) || isArrayBufferView(val)) {
    return;
  }

  throw new ERR_INVALID_ARG_TYPE(
    name,
    ["string", "ArrayBuffer", "TypedArray", "DataView", "Buffer"],
    val,
  );
});

// Mirrors the (canonical) curve names exposed by Node.js's `getCurves()`.
// Notably this drops `secp256r1` because it is just another name for
// `prime256v1` and exposing both confuses callers that probe for an
// unsupported curve by exclusion (e.g. crypto tests).
// Must be kept in sorted (lexicographic) order - Node.js validates this.
const curveNames: readonly string[] = [
  "brainpoolP256r1",
  "brainpoolP384r1",
  "prime256v1",
  "secp224r1",
  "secp256k1",
  "secp384r1",
  "secp521r1",
];
function getCurves(): string[] {
  return ArrayPrototypeSlice(curveNames);
}

interface SecureHeapUsage {
  total: number;
  min: number;
  used: number;
  utilization: number;
}

function secureHeapUsed(): SecureHeapUsage {
  notImplemented("crypto.secureHeapUsed");
}

function setEngine(_engine: string, _flags: typeof constants) {
  notImplemented("crypto.setEngine");
}

function getOpenSSLSecLevel(): number {
  return 5; // highest sec level, used in tests.
}

const kAesKeyLengths = [128, 192, 256];

const _defaultExport = {
  getDefaultEncoding,
  setDefaultEncoding,
  getCiphers,
  getCipherInfo,
  getCurves,
  getHashBlockSize,
  getOpenSSLSecLevel,
  secureHeapUsed,
  setEngine,
  validateByteSource,
  toBuf,
  kHandle,
  kKeyObject,
  kAesKeyLengths,
};

return {
  ellipticCurves,
  getCiphers,
  getHashBlockSize,
  getCipherInfo,
  setDefaultEncoding,
  getDefaultEncoding,
  toBuf,
  validateByteSource,
  getCurves,
  secureHeapUsed,
  setEngine,
  getOpenSSLSecLevel,
  kAesKeyLengths,
  kHandle,
  kKeyObject,
  default: _defaultExport,
};
})();
