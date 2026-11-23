// Copyright 2018-2026 the Deno authors. MIT license.

// Writes the RSA-PSS keys crypto_key_test.ts compares node:crypto's PKCS#8
// and SPKI exports with. Run it with Node from the repository root:
//   node tests/unit_node/crypto/generate_rsa_pss_fixtures.mjs
// Node's OpenSSL writes the RSASSA-PSS-params with a field at its default
// left out and NULL parameters on the hash AlgorithmIdentifiers.

import { writeFileSync } from "node:fs";
import { join } from "node:path";
import crypto from "node:crypto";

const dir = join("tests", "unit_node", "crypto", "testdata", "rsa_pss");
const cases = {
  // No RSASSA-PSS-params at all.
  unrestricted: {},
  // Every field at its default: an empty RSASSA-PSS-params.
  defaults: {
    hashAlgorithm: "sha1",
    mgf1HashAlgorithm: "sha1",
    saltLength: 20,
  },
  // sha256 for both hashes and a 32-byte salt.
  sha256: { hashAlgorithm: "sha256" },
  // A different MGF1 hash and salt length.
  sha512_sha256_16: {
    hashAlgorithm: "sha512",
    mgf1HashAlgorithm: "sha256",
    saltLength: 16,
  },
  // MGF1 at its default (SHA-1) under another hash: no maskGenAlgorithm.
  sha384_mgf1_sha1_0: {
    hashAlgorithm: "sha384",
    mgf1HashAlgorithm: "sha1",
    saltLength: 0,
  },
  // Only the salt length differs from the defaults.
  salt32: { saltLength: 32 },
};

const details = {};
for (const [name, options] of Object.entries(cases)) {
  const { privateKey, publicKey } = crypto.generateKeyPairSync("rsa-pss", {
    modulusLength: 1024,
    ...options,
  });
  writeFileSync(
    join(dir, `${name}.pkcs8.pem`),
    privateKey.export({ type: "pkcs8", format: "pem" }),
  );
  writeFileSync(
    join(dir, `${name}.spki.pem`),
    publicKey.export({ type: "spki", format: "pem" }),
  );
  const { publicExponent, ...rest } = privateKey.asymmetricKeyDetails;
  details[name] = { ...rest, publicExponent: Number(publicExponent) };
}
writeFileSync(
  join(dir, "details.json"),
  JSON.stringify(details, null, 2) + "\n",
);
