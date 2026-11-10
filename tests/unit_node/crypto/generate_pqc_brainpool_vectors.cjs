// Copyright 2018-2026 the Deno authors. MIT license.
// Writes pqc_brainpool_vectors.json with real Node.js (OpenSSL 3.5+):
// brainpool keys, ECDH secrets and ECDSA signatures on all three brainpool
// curves, and ML-DSA / ML-KEM keys with a signature and a ciphertext, for
// crypto_pqc_brainpool_test.ts. Run: node generate_pqc_brainpool_vectors.cjs
"use strict";
const c = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const message = Buffer.from("denext brainpool and PQC vectors");
const brainpool = [];
for (const curve of ["brainpoolP256r1", "brainpoolP384r1", "brainpoolP512r1"]) {
  const n =
    { brainpoolP256r1: 32, brainpoolP384r1: 48, brainpoolP512r1: 64 }[curve];
  const a = c.createECDH(curve);
  const b = c.createECDH(curve);
  a.setPrivateKey(Buffer.alloc(n, 0x11));
  b.setPrivateKey(Buffer.alloc(n, 0x22));
  const { privateKey, publicKey } = c.generateKeyPairSync("ec", {
    namedCurve: curve,
  });
  brainpool.push({
    curve,
    aPub: a.getPublicKey("hex"),
    bPub: b.getPublicKey("hex"),
    aPubC: a.getPublicKey("hex", "compressed"),
    secret: a.computeSecret(b.getPublicKey()).toString("hex"),
    pkcs8: privateKey.export({ type: "pkcs8", format: "pem" }),
    sec1: privateKey.export({ type: "sec1", format: "pem" }),
    spki: publicKey.export({ type: "spki", format: "pem" }),
    sigDer: c.sign("sha256", message, privateKey).toString("hex"),
    sigP1363: c.sign("sha384", message, {
      key: privateKey,
      dsaEncoding: "ieee-p1363",
    }).toString("hex"),
  });
}

// ECDSA with a digest shorter than half the field (P-384 / SHA-1,
// P-521 / SHA-256), which OpenSSL signs and verifies.
const shortDigest = [];
for (const [namedCurve, hash] of [["P-384", "sha1"], ["P-521", "sha256"]]) {
  const { privateKey, publicKey } = c.generateKeyPairSync("ec", { namedCurve });
  shortDigest.push({
    hash,
    spki: publicKey.export({ type: "spki", format: "pem" }),
    pkcs8: privateKey.export({ type: "pkcs8", format: "pem" }),
    signature: c.sign(hash, message, privateKey).toString("hex"),
  });
}

const pqc = [];
for (const type of ["ml-dsa-44", "ml-dsa-65", "ml-dsa-87"]) {
  const { privateKey, publicKey } = c.generateKeyPairSync(type);
  pqc.push({
    type,
    seed: privateKey.export({ format: "raw-seed" }).toString("hex"),
    pub: publicKey.export({ format: "raw-public" }).toString("hex"),
    pkcs8: privateKey.export({ type: "pkcs8", format: "pem" }),
    spki: publicKey.export({ type: "spki", format: "pem" }),
    signature: c.sign(null, message, privateKey).toString("hex"),
  });
}
for (const type of ["ml-kem-768", "ml-kem-1024"]) {
  const { privateKey, publicKey } = c.generateKeyPairSync(type);
  const { sharedKey, ciphertext } = c.encapsulate(publicKey);
  pqc.push({
    type,
    seed: privateKey.export({ format: "raw-seed" }).toString("hex"),
    pub: publicKey.export({ format: "raw-public" }).toString("hex"),
    pkcs8: privateKey.export({ type: "pkcs8", format: "pem" }),
    spki: publicKey.export({ type: "spki", format: "pem" }),
    ciphertext: ciphertext.toString("hex"),
    sharedKey: sharedKey.toString("hex"),
  });
}

fs.writeFileSync(
  path.join(__dirname, "pqc_brainpool_vectors.json"),
  JSON.stringify(
    {
      generator:
        `node generate_pqc_brainpool_vectors.cjs (Node ${process.version})`,
      message: message.toString("hex"),
      brainpool,
      shortDigest,
      pqc,
    },
    null,
    1,
  ) + "\n",
);
