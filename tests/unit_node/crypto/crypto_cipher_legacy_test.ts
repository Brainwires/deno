// Copyright 2018-2026 the Deno authors. MIT license.
// The legacy node:crypto ciphers and the brainpool ECDH curves (DENEXT.md
// item NNN), against vectors Node.js produced (legacy_cipher_vectors.json,
// written by generate_legacy_vectors.cjs).
import crypto from "node:crypto";
import { Buffer } from "node:buffer";
import { assert, assertEquals, assertThrows } from "@std/assert";

interface CipherVector {
  name: string;
  key: string;
  iv: string;
  ct: string;
  tag?: string;
  aad?: string;
}

interface EcdhVector {
  curve: string;
  aPub: string;
  bPub: string;
  aPubC: string;
  secret: string;
}

const vectors: { ciphers: CipherVector[]; ecdh: EcdhVector[] } = JSON.parse(
  Deno.readTextFileSync(
    new URL("./legacy_cipher_vectors.json", import.meta.url),
  ),
);

const plaintext = Buffer.from(
  "The quick brown fox jumps over the lazy dog, 51 bytes",
);

function hex(s: string): Buffer {
  return Buffer.from(s, "hex");
}

for (const v of vectors.ciphers) {
  Deno.test(`[node/crypto] legacy cipher ${v.name} matches Node`, () => {
    const key = hex(v.key);
    const iv = v.iv ? hex(v.iv) : null;
    const options = v.tag ? { authTagLength: 16 } : undefined;
    const aad = v.aad ? hex(v.aad) : undefined;

    const cipher = crypto.createCipheriv(v.name, key, iv, options);
    if (aad) cipher.setAAD(aad, { plaintextLength: plaintext.length });
    const ct = Buffer.concat([cipher.update(plaintext), cipher.final()]);
    assertEquals(ct.toString("hex"), v.ct);
    if (v.tag) assertEquals(cipher.getAuthTag().toString("hex"), v.tag);

    const decipher = crypto.createDecipheriv(v.name, key, iv, options);
    if (v.tag) decipher.setAuthTag(hex(v.tag));
    if (aad) decipher.setAAD(aad, { plaintextLength: plaintext.length });
    const pt = Buffer.concat([decipher.update(hex(v.ct)), decipher.final()]);
    assertEquals(pt.toString(), plaintext.toString());
  });
}

Deno.test("[node/crypto] legacy ciphers stream in small chunks", () => {
  for (const v of vectors.ciphers) {
    // CCM takes its message in one update, as in Node; XTS treats each
    // update as one data unit
    if (v.name.endsWith("-ccm") || v.name.endsWith("-xts")) continue;
    const options = v.tag ? { authTagLength: 16 } : undefined;
    const cipher = crypto.createCipheriv(
      v.name,
      hex(v.key),
      v.iv ? hex(v.iv) : null,
      options,
    );
    if (v.aad) cipher.setAAD(hex(v.aad));
    const parts = [];
    for (let i = 0; i < plaintext.length; i += 5) {
      parts.push(cipher.update(plaintext.subarray(i, i + 5)));
    }
    parts.push(cipher.final());
    assertEquals(Buffer.concat(parts).toString("hex"), v.ct, v.name);
  }
});

Deno.test("[node/crypto] getCiphers() lists only ciphers that work", () => {
  // distinct bytes: XTS refuses a key whose halves are equal
  const key = (n: number) =>
    Buffer.from(Array.from({ length: n }, (_, i) => i));
  for (const name of crypto.getCiphers()) {
    if (name.includes("wrap")) continue;
    const info = crypto.getCipherInfo(name);
    assert(info, name);
    const aead = ["gcm", "ccm", "ocb"].includes(info.mode) ||
      name === "chacha20-poly1305";
    const iv = aead
      ? Buffer.alloc(12, 1)
      : info.ivLength
      ? Buffer.alloc(info.ivLength, 1)
      : null;
    const options = aead ? { authTagLength: 16 } : undefined;
    const cipher = crypto.createCipheriv(
      name,
      key(info.keyLength),
      iv,
      options,
    );
    // XTS: a data unit is at least one block
    const data = Buffer.alloc(32, 7);
    const ct = Buffer.concat([cipher.update(data), cipher.final()]);
    const decipher = crypto.createDecipheriv(
      name,
      key(info.keyLength),
      iv,
      options,
    );
    if (aead) decipher.setAuthTag(cipher.getAuthTag());
    const pt = Buffer.concat([decipher.update(ct), decipher.final()]);
    assertEquals(pt, data, name);
  }
});

Deno.test("[node/crypto] CCM and OCB parameters as in Node", () => {
  const key = Buffer.alloc(16, 1);
  assertThrows(
    () => crypto.createCipheriv("aes-128-ccm", key, Buffer.alloc(12)),
    TypeError,
    "authTagLength required for aes-128-ccm",
  );
  assertThrows(
    () => crypto.createCipheriv("aes-128-ocb", key, Buffer.alloc(12)),
    TypeError,
    "authTagLength required for aes-128-ocb",
  );
  const ccm = crypto.createCipheriv("aes-128-ccm", key, Buffer.alloc(12), {
    authTagLength: 16,
  });
  assertThrows(
    () => ccm.setAAD(Buffer.from("a")),
    TypeError,
    "options.plaintextLength required for CCM mode with AAD",
  );
  ccm.update("a");
  assertThrows(() => ccm.update("b"), Error, "unsupported state");

  // a CCM tag that doesn't authenticate: no plaintext, and final() throws
  const decipher = crypto.createDecipheriv(
    "aes-128-ccm",
    key,
    Buffer.alloc(12),
    { authTagLength: 16 },
  );
  decipher.setAuthTag(Buffer.alloc(16));
  assertEquals(decipher.update(Buffer.from("abc")).length, 0);
  assertThrows(
    () => decipher.final(),
    Error,
    "Unsupported state or unable to authenticate data",
  );

  // OCB with a wrong tag fails in final(), as Node's "bad decrypt"
  const ocb = crypto.createDecipheriv("aes-128-ocb", key, Buffer.alloc(12), {
    authTagLength: 16,
  });
  ocb.setAuthTag(Buffer.alloc(16));
  ocb.update(Buffer.alloc(40));
  assertThrows(() => ocb.final(), Error, "bad decrypt");
});

Deno.test("[node/crypto] XTS refuses duplicated keys and short units", () => {
  const iv = Buffer.alloc(16, 3);
  assertThrows(
    () => crypto.createCipheriv("aes-128-xts", Buffer.alloc(32, 1), iv),
    Error,
    "xts duplicated keys",
  );
  const key = Buffer.concat([Buffer.alloc(16, 1), Buffer.alloc(16, 9)]);
  assertThrows(
    () => crypto.createCipheriv("aes-128-xts", key, iv).update(Buffer.alloc(8)),
    Error,
    "unsupported state",
  );
  // Node: 20 bytes with ciphertext stealing
  const x = crypto.createCipheriv("aes-128-xts", key, iv);
  assertEquals(
    Buffer.concat([x.update(Buffer.alloc(20, 5)), x.final()]).toString("hex"),
    "aa84ef3d7be81171a3e5f432fba9d447371207fa",
  );
});

Deno.test("[node/crypto] ciphers Node 26 refuses stay unknown", () => {
  for (const name of ["des-cbc", "bf-cbc", "cast5-cbc", "rc4", "rc2-cbc"]) {
    assert(!crypto.getCiphers().includes(name), name);
    assertThrows(
      () => crypto.createCipheriv(name, Buffer.alloc(16), Buffer.alloc(8)),
      Error,
    );
  }
});

for (const v of vectors.ecdh) {
  Deno.test(`[node/crypto] ECDH on ${v.curve} matches Node`, () => {
    assert(crypto.getCurves().includes(v.curve));
    const n = v.curve.endsWith("256r1") ? 32 : 48;
    const a = crypto.createECDH(v.curve);
    const b = crypto.createECDH(v.curve);
    a.setPrivateKey(Buffer.alloc(n, 0x11));
    b.setPrivateKey(Buffer.alloc(n, 0x22));
    assertEquals(a.getPublicKey("hex"), v.aPub);
    assertEquals(b.getPublicKey("hex"), v.bPub);
    assertEquals(a.getPublicKey("hex", "compressed"), v.aPubC);
    assertEquals(a.computeSecret(hex(v.bPub)).toString("hex"), v.secret);
    assertEquals(b.computeSecret(hex(v.aPub)).toString("hex"), v.secret);
    assertEquals(
      crypto.ECDH.convertKey(v.aPubC, v.curve, "hex", "hex", "uncompressed"),
      v.aPub,
    );
    // fresh keys agree too
    const c = crypto.createECDH(v.curve);
    const d = crypto.createECDH(v.curve);
    c.generateKeys();
    d.generateKeys();
    assertEquals(
      c.computeSecret(d.getPublicKey()),
      d.computeSecret(c.getPublicKey()),
    );
  });
}

Deno.test("[node/crypto] getCurves() is sorted", () => {
  const curves = crypto.getCurves();
  assertEquals([...curves].sort(), curves);
});
