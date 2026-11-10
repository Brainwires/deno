// Copyright 2018-2026 the Deno authors. MIT license.
// brainpoolP512r1, brainpool KeyObjects (generateKeyPair, PKCS#8 / SEC1 /
// SPKI, ECDSA, diffieHellman) and ML-DSA / ML-KEM KeyObjects, against values
// real Node.js produced (pqc_brainpool_vectors.json, written by
// generate_pqc_brainpool_vectors.cjs).
// deno-lint-ignore-file no-explicit-any
import nodeCrypto from "node:crypto";
import { Buffer } from "node:buffer";
import { assert, assertEquals, assertThrows } from "@std/assert";

interface BrainpoolVector {
  curve: string;
  aPub: string;
  bPub: string;
  aPubC: string;
  secret: string;
  pkcs8: string;
  sec1: string;
  spki: string;
  sigDer: string;
  sigP1363: string;
}

interface PqcVector {
  type: string;
  seed: string;
  pub: string;
  pkcs8: string;
  spki: string;
  signature?: string;
  ciphertext?: string;
  sharedKey?: string;
}

const vectors: {
  message: string;
  brainpool: BrainpoolVector[];
  shortDigest: {
    hash: string;
    spki: string;
    pkcs8: string;
    signature: string;
  }[];
  pqc: PqcVector[];
} = JSON.parse(
  Deno.readTextFileSync(
    new URL("./pqc_brainpool_vectors.json", import.meta.url),
  ),
);
// @types/node does not describe raw-seed, ML-DSA / ML-KEM or encapsulate yet.
const crypto: any = nodeCrypto;
const message = Buffer.from(vectors.message, "hex");
const hex = (s: string) => Buffer.from(s, "hex");
const fieldLen: Record<string, number> = {
  brainpoolP256r1: 32,
  brainpoolP384r1: 48,
  brainpoolP512r1: 64,
};

for (const v of vectors.shortDigest) {
  Deno.test(`[node/crypto] ECDSA with ${v.hash} on a larger curve, as OpenSSL`, () => {
    const publicKey = crypto.createPublicKey(v.spki);
    const privateKey = crypto.createPrivateKey(v.pkcs8);
    assert(crypto.verify(v.hash, message, publicKey, hex(v.signature)));
    const sig = crypto.sign(v.hash, message, privateKey);
    assert(crypto.verify(v.hash, message, publicKey, sig));
    assert(!crypto.verify(v.hash, Buffer.from("x"), publicKey, sig));
  });
}

Deno.test("[node/crypto] getCurves() lists brainpoolP512r1", () => {
  assert(crypto.getCurves().includes("brainpoolP512r1"));
});

for (const v of vectors.brainpool) {
  const n = fieldLen[v.curve];

  Deno.test(`[node/crypto] createECDH(${v.curve}) matches Node`, () => {
    const a = crypto.createECDH(v.curve);
    const b = crypto.createECDH(v.curve);
    a.setPrivateKey(Buffer.alloc(n, 0x11));
    b.setPrivateKey(Buffer.alloc(n, 0x22));
    assertEquals(a.getPublicKey("hex"), v.aPub);
    assertEquals(b.getPublicKey("hex"), v.bPub);
    assertEquals(a.getPublicKey("hex", "compressed"), v.aPubC);
    assertEquals(a.computeSecret(hex(v.bPub)).toString("hex"), v.secret);
    assertEquals(
      crypto.ECDH.convertKey(v.aPubC, v.curve, "hex", "hex", "uncompressed"),
      v.aPub,
    );
    const c = crypto.createECDH(v.curve);
    const d = crypto.createECDH(v.curve);
    c.generateKeys();
    d.generateKeys();
    assertEquals(c.getPrivateKey().length, n);
    assertEquals(
      c.computeSecret(d.getPublicKey()),
      d.computeSecret(c.getPublicKey()),
    );
  });

  Deno.test(`[node/crypto] ${v.curve} KeyObjects read and write Node's encodings`, () => {
    const fromPkcs8 = crypto.createPrivateKey(v.pkcs8);
    const fromSec1 = crypto.createPrivateKey(v.sec1);
    const publicKey = crypto.createPublicKey(v.spki);
    assertEquals(fromPkcs8.asymmetricKeyType, "ec");
    assertEquals(fromPkcs8.asymmetricKeyDetails, { namedCurve: v.curve });
    assertEquals(publicKey.asymmetricKeyDetails, { namedCurve: v.curve });
    assert(fromPkcs8.equals(fromSec1));
    assertEquals(fromPkcs8.export({ type: "pkcs8", format: "pem" }), v.pkcs8);
    assertEquals(fromPkcs8.export({ type: "sec1", format: "pem" }), v.sec1);
    assertEquals(publicKey.export({ type: "spki", format: "pem" }), v.spki);
    assert(crypto.createPublicKey(fromPkcs8).equals(publicKey));
    // Node's signatures verify, in DER and in IEEE P1363.
    assert(crypto.verify("sha256", message, publicKey, hex(v.sigDer)));
    assert(
      crypto.verify(
        "sha384",
        message,
        { key: publicKey, dsaEncoding: "ieee-p1363" },
        hex(v.sigP1363),
      ),
    );
    assert(
      !crypto.verify("sha256", Buffer.from("x"), publicKey, hex(v.sigDer)),
    );
    // JWK has no brainpool curves, as in Node.
    assertThrows(
      () => publicKey.export({ format: "jwk" }),
      Error,
      `Unsupported JWK EC curve: ${v.curve}.`,
    );
  });

  Deno.test(`[node/crypto] generateKeyPair("ec", ${v.curve})`, async () => {
    const { privateKey, publicKey } = crypto.generateKeyPairSync("ec", {
      namedCurve: v.curve,
    });
    assertEquals(privateKey.asymmetricKeyDetails, { namedCurve: v.curve });
    const raw = publicKey.export({ format: "raw-public" });
    assertEquals(raw.length, 1 + 2 * n);
    assertEquals(
      publicKey.export({ format: "raw-public", type: "compressed" }).length,
      1 + n,
    );
    assertEquals(privateKey.export({ format: "raw-private" }).length, n);
    const back = crypto.createPrivateKey({
      key: privateKey.export({ type: "pkcs8", format: "der" }),
      format: "der",
      type: "pkcs8",
    });
    assert(back.equals(privateKey));
    const fromRaw = crypto.createPublicKey({
      key: raw,
      format: "raw-public",
      asymmetricKeyType: "ec",
      namedCurve: v.curve,
    });
    assert(fromRaw.equals(publicKey));
    for (const dsaEncoding of ["der", "ieee-p1363"]) {
      const sig = crypto.sign("sha256", message, {
        key: privateKey,
        dsaEncoding,
      });
      if (dsaEncoding === "ieee-p1363") assertEquals(sig.length, 2 * n);
      assert(
        crypto.verify("sha256", message, { key: publicKey, dsaEncoding }, sig),
      );
    }
    // diffieHellman() over KeyObjects agrees with createECDH.
    const other = crypto.generateKeyPairSync("ec", { namedCurve: v.curve });
    assertEquals(
      crypto.diffieHellman({ privateKey, publicKey: other.publicKey }),
      crypto.diffieHellman({
        privateKey: other.privateKey,
        publicKey,
      }),
    );
    const asyncPair = await new Promise<any>(
      (resolve, reject) =>
        crypto.generateKeyPair(
          "ec",
          { namedCurve: v.curve },
          (err: Error | null, publicKey: any, privateKey: any) =>
            err ? reject(err) : resolve({ publicKey, privateKey }),
        ),
    );
    assertEquals(asyncPair.privateKey.asymmetricKeyDetails, {
      namedCurve: v.curve,
    });
  });
}

for (const v of vectors.pqc) {
  Deno.test(`[node/crypto] ${v.type} KeyObjects read Node's keys`, () => {
    const privateKey = crypto.createPrivateKey(v.pkcs8);
    const publicKey = crypto.createPublicKey(v.spki);
    assertEquals(privateKey.asymmetricKeyType, v.type);
    assertEquals(publicKey.asymmetricKeyDetails, {});
    assertEquals(privateKey.export({ type: "pkcs8", format: "pem" }), v.pkcs8);
    assertEquals(publicKey.export({ type: "spki", format: "pem" }), v.spki);
    assertEquals(
      privateKey.export({ format: "raw-seed" }).toString("hex"),
      v.seed,
    );
    assertEquals(
      publicKey.export({ format: "raw-public" }).toString("hex"),
      v.pub,
    );
    assert(crypto.createPublicKey(privateKey).equals(publicKey));
    const jwk = privateKey.export({ format: "jwk" });
    assertEquals(jwk.kty, "AKP");
    assert(
      crypto.createPrivateKey({ key: jwk, format: "jwk" }).equals(privateKey),
    );
    if (v.signature) {
      assert(crypto.verify(null, message, publicKey, hex(v.signature)));
      const sig = crypto.sign(null, message, privateKey);
      assert(crypto.verify(null, message, publicKey, sig));
      assert(!crypto.verify(null, Buffer.from("x"), publicKey, sig));
    }
    if (v.ciphertext) {
      assertEquals(
        crypto.decapsulate(privateKey, hex(v.ciphertext)).toString("hex"),
        v.sharedKey,
      );
      const { sharedKey, ciphertext } = crypto.encapsulate(publicKey);
      assertEquals(crypto.decapsulate(privateKey, ciphertext), sharedKey);
    }
  });

  Deno.test(`[node/crypto] generateKeyPair(${v.type})`, () => {
    const { privateKey, publicKey } = crypto.generateKeyPairSync(v.type);
    assertEquals(privateKey.asymmetricKeyType, v.type);
    assertEquals(
      publicKey.export({ format: "raw-public" }).length,
      v.pub.length / 2,
    );
    assertEquals(
      privateKey.export({ format: "raw-seed" }).length,
      v.seed.length / 2,
    );
  });
}

Deno.test("[node/crypto] ML-KEM-512 is refused as Node's BoringSSL build does", () => {
  assertThrows(() => crypto.generateKeyPairSync("ml-kem-512"));
});
