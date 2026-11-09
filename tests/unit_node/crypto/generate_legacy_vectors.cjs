// Copyright 2018-2026 the Deno authors. MIT license.
// Writes legacy_cipher_vectors.json with Node.js (run it with `node`, not
// Deno): one encryption per cipher of ext/node_crypto/cipher_legacy.rs, and
// ECDH key pairs and secrets on the brainpool curves (DENEXT.md item 292).
const c = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const names = process.argv.slice(2);
const pt = Buffer.from("The quick brown fox jumps over the lazy dog, 51 bytes");
const ciphers = [];
for (const name of names) {
  const info = c.getCipherInfo(name);
  const key = Buffer.alloc(info.keyLength);
  for (let i = 0; i < key.length; i++) key[i] = (i * 7 + 1) & 0xff;
  const aead = ["ccm", "ocb", "gcm"].includes(info.mode);
  const iv = Buffer.alloc(aead ? 12 : (info.ivLength ?? 0));
  for (let i = 0; i < iv.length; i++) iv[i] = (i * 13 + 5) & 0xff;
  const opts = aead ? { authTagLength: 16 } : undefined;
  const x = c.createCipheriv(name, key, iv.length ? iv : null, opts);
  const aad = Buffer.from("header");
  if (aead) x.setAAD(aad, { plaintextLength: pt.length });
  const ct = Buffer.concat([x.update(pt), x.final()]);
  ciphers.push({
    name,
    key: key.toString("hex"),
    iv: iv.toString("hex"),
    ct: ct.toString("hex"),
    tag: aead ? x.getAuthTag().toString("hex") : undefined,
    aad: aead ? aad.toString("hex") : undefined,
  });
}
const ecdh = [];
for (const curve of ["brainpoolP256r1", "brainpoolP384r1"]) {
  const n = curve.endsWith("256r1") ? 32 : 48;
  const a = c.createECDH(curve);
  const b = c.createECDH(curve);
  a.setPrivateKey(Buffer.alloc(n, 0x11));
  b.setPrivateKey(Buffer.alloc(n, 0x22));
  ecdh.push({
    curve,
    aPub: a.getPublicKey("hex"),
    bPub: b.getPublicKey("hex"),
    aPubC: a.getPublicKey("hex", "compressed"),
    secret: a.computeSecret(b.getPublicKey()).toString("hex"),
  });
}
fs.writeFileSync(
  path.join(__dirname, "legacy_cipher_vectors.json"),
  JSON.stringify(
    {
      generator: "node generate_legacy_vectors.cjs (Node 26.13.0)",
      ciphers,
      ecdh,
    },
    null,
    1,
  ) + "\n",
);
