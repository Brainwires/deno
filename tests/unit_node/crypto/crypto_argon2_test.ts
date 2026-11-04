// Copyright 2018-2026 the Deno authors. MIT license.
import crypto, {
  argon2,
  type Argon2Algorithm as Algorithm,
  type Argon2Parameters as Parameters,
  argon2Sync,
} from "node:crypto";
import { Buffer } from "node:buffer";
import { assert, assertEquals, assertThrows } from "@std/assert";

const message = Buffer.alloc(32, 0x01);
const nonce = Buffer.alloc(16, 0x02);
const secret = Buffer.alloc(8, 0x03);
const associatedData = Buffer.alloc(12, 0x04);
const defaults = {
  message,
  nonce,
  parallelism: 1,
  tagLength: 64,
  memory: 8,
  passes: 3,
};

function argon2Async(
  algorithm: Algorithm,
  parameters: Parameters,
): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    argon2(algorithm, parameters, (err, key) => {
      if (err) reject(err);
      else resolve(key);
    });
  });
}

// Node's test/parallel/test-crypto-argon2.js: RFC 9106 section 5 and
// OpenSSL 3.2's evpkdf_argon2.txt.
const knownAnswers: [Algorithm, Partial<Parameters>, string][] = [
  [
    "argon2d",
    { secret, associatedData, parallelism: 4, tagLength: 32, memory: 32 },
    "512b391b6f1162975371d30919734294f868e3be3984f3c1a13a4db9fabe4acb",
  ],
  [
    "argon2i",
    { secret, associatedData, parallelism: 4, tagLength: 32, memory: 32 },
    "c814d9d1dc7f37aa13f0d77f2494bda1c8de6b016dd388d29952a4c4672b6ce8",
  ],
  [
    "argon2id",
    { secret, associatedData, parallelism: 4, tagLength: 32, memory: 32 },
    "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659",
  ],
  [
    "argon2d",
    { message: "1234567890", nonce: "saltsalt" },
    "d16ad773b1c6400d3193bc3e66271603e9de72bace20af3f89c236f5434cdec9" +
    "9072ddfc6b9c77ea9f386c0e8d7cb0c37cec6ec3277a22c92d5be58ef67c7eaa",
  ],
  [
    "argon2id",
    { message: "", parallelism: 4, tagLength: 32, memory: 32 },
    "0a34f1abde67086c82e785eaf17c68382259a264f4e61b91cd2763cb75ac189a",
  ],
  [
    "argon2d",
    { message: "1234567890", nonce: "saltsalt", parallelism: 2, memory: 65536 },
    "5ca0ab135de1241454840172696c301c7b8fd99a788cd11cf9699044cadf7fca" +
    "0a6e3762cb3043a71adf6553db3fd7925101b0ccf8868b098492a4addb2486bc",
  ],
  [
    "argon2i",
    { parallelism: 4, tagLength: 32, memory: 32 },
    "a9a7510e6db4d588ba3414cd0e094d480d683f97b9ccb612a544fe8ef65ba8e0",
  ],
  [
    "argon2id",
    { parallelism: 4, tagLength: 32, memory: 32 },
    "03aab965c12001c9d7d0d2de33192c0494b684bb148196d73c1df1acaf6d0c2e",
  ],
  [
    "argon2d",
    {
      message: "1234567890",
      nonce: "saltsalt",
      parallelism: 2,
      tagLength: 128,
      memory: 65536,
    },
    "a86c83a19f0b234ecba8c275d16d059153f961e4c39ec9b1be98b3e73d791789" +
    "363682443ad594334048634e91c493affed0bc29fd329a0e553c00149d6db19a" +
    "f4e4a354aec14dbd575d78ba87d4a4bc4746666e7a4e6ee1572bbffc2eba308a" +
    "2d825cb7b41fde3a95d5cff0dfa2d0fdd636b32aea8b4a3c532742d330bd1b90",
  ],
];

Deno.test("[node/crypto] argon2Sync and argon2 match the known answers", async () => {
  for (const [algorithm, overrides, expected] of knownAnswers) {
    const parameters = { ...defaults, ...overrides };
    const sync = argon2Sync(algorithm, parameters);
    assert(Buffer.isBuffer(sync));
    assertEquals(sync.toString("hex"), expected);
    const async = await argon2Async(algorithm, parameters);
    assert(Buffer.isBuffer(async));
    assertEquals(async.toString("hex"), expected);
  }
});

Deno.test("[node/crypto] argon2 is on the default export too", () => {
  assertEquals(crypto.argon2, argon2);
  assertEquals(crypto.argon2Sync, argon2Sync);
});

Deno.test("[node/crypto] argon2 calls back asynchronously, with the key", async () => {
  let called = false;
  const done = new Promise<Buffer>((resolve, reject) => {
    argon2("argon2id", defaults, (err, key) => {
      called = true;
      if (err) reject(err);
      else resolve(key);
    });
  });
  assertEquals(called, false);
  assertEquals((await done).length, 64);
});

Deno.test("[node/crypto] argon2 omitted and empty secret / associatedData agree", () => {
  const omitted = argon2Sync("argon2id", defaults);
  const empty = argon2Sync("argon2id", {
    ...defaults,
    secret: Buffer.alloc(0),
    associatedData: Buffer.alloc(0),
  });
  assertEquals(omitted, empty);
});

Deno.test("[node/crypto] argon2 takes strings, ArrayBuffers and every view", () => {
  const expected = argon2Sync("argon2id", {
    ...defaults,
    message: Buffer.from("password"),
    nonce: Buffer.from("saltsaltsalt"),
    secret,
    associatedData,
  });
  const ab = (b: Buffer): ArrayBuffer =>
    new Uint8Array(b).buffer as ArrayBuffer;
  const variants: Partial<Parameters>[] = [
    { message: "password", nonce: "saltsaltsalt" },
    {
      message: ab(Buffer.from("password")),
      nonce: new DataView(ab(Buffer.from("saltsaltsalt"))),
      secret: new Uint8Array(ab(secret)),
      associatedData: new DataView(ab(associatedData)),
    },
    {
      // A view at an offset into a larger buffer.
      message: Buffer.from("xxpasswordyy").subarray(2, 10),
      nonce: new Uint16Array(ab(Buffer.from("saltsaltsalt"))),
      secret: ab(secret),
      associatedData: ab(associatedData),
    },
  ];
  for (const variant of variants) {
    const actual = argon2Sync("argon2id", {
      ...defaults,
      secret,
      associatedData,
      ...variant,
    });
    assertEquals(actual, expected);
  }
});

Deno.test("[node/crypto] argon2 range errors match Node's", () => {
  const bad: [Partial<Parameters>, string][] = [
    [{ nonce: nonce.subarray(0, 7) }, "parameters.nonce.byteLength"],
    [{ tagLength: 3 }, "parameters.tagLength"],
    [{ tagLength: 2 ** 32 }, "parameters.tagLength"],
    [{ passes: 0 }, "parameters.passes"],
    [{ passes: 2 ** 32 }, "parameters.passes"],
    [{ parallelism: 0 }, "parameters.parallelism"],
    [{ parallelism: 2 ** 24 }, "parameters.parallelism"],
    [{ parallelism: 4, memory: 16 }, "parameters.memory"],
    // The bound on working memory: at most 2^32 - 1 KiB.
    [{ memory: 2 ** 32 }, "parameters.memory"],
    [{ parallelism: 1.5 }, "parameters.parallelism"],
  ];
  for (const [overrides, name] of bad) {
    const parameters = { ...defaults, ...overrides };
    for (
      const call of [
        () => argon2("argon2id", parameters, () => {}),
        () => argon2Sync("argon2id", parameters),
      ]
    ) {
      const err = assertThrows(call, RangeError);
      // deno-lint-ignore no-explicit-any
      assertEquals((err as any).code, "ERR_OUT_OF_RANGE");
      assert(
        (err as Error).message.includes(
          `The value of "${name}" is out of range`,
        ),
        (err as Error).message,
      );
    }
  }
});

Deno.test("[node/crypto] argon2 type errors match Node's", () => {
  for (const key of Object.keys(defaults)) {
    const parameters: Record<string, unknown> = { ...defaults };
    delete parameters[key];
    for (
      const call of [
        () => argon2("argon2id", parameters as unknown as Parameters, () => {}),
        () => argon2Sync("argon2id", parameters as unknown as Parameters),
      ]
    ) {
      const err = assertThrows(call, TypeError);
      // deno-lint-ignore no-explicit-any
      assertEquals((err as any).code, "ERR_INVALID_ARG_TYPE");
      assert(
        (err as Error).message.includes(`"parameters.${key}"`),
        (err as Error).message,
      );
    }
  }
  // deno-lint-ignore no-explicit-any
  const anyArgon2 = argon2 as any;
  for (
    const call of [
      () => anyArgon2(),
      () => anyArgon2("argon2id", null),
      () => anyArgon2("argon2id", defaults, null),
      () => anyArgon2("argon2id", defaults, {}),
    ]
  ) {
    const err = assertThrows(call, TypeError);
    // deno-lint-ignore no-explicit-any
    assertEquals((err as any).code, "ERR_INVALID_ARG_TYPE");
  }
  const err = assertThrows(
    // deno-lint-ignore no-explicit-any
    () => argon2Sync("argon2x" as any, defaults),
    TypeError,
  );
  // deno-lint-ignore no-explicit-any
  assertEquals((err as any).code, "ERR_INVALID_ARG_VALUE");
});

Deno.test("[node/crypto] argon2 refuses associatedData the KDF cannot take", async () => {
  const parameters = { ...defaults, associatedData: Buffer.alloc(33, 0x04) };
  assertThrows(
    () => argon2Sync("argon2id", parameters),
    Error,
    "associatedData longer than 32 bytes is not supported",
  );
  let err: Error | undefined;
  try {
    await argon2Async("argon2id", parameters);
  } catch (e) {
    err = e as Error;
  }
  assert(err?.message.includes("associatedData longer than 32 bytes"));
  // 32 bytes is fine.
  assertEquals(
    argon2Sync("argon2id", { ...defaults, associatedData: Buffer.alloc(32) })
      .length,
    64,
  );
});
