// Writes a revoked-runtimes list revoking this very build, signed by a fresh
// test key, where the CLI reads denext's cached copy (`<DENO_DIR>/denext-deno`),
// and the key where `DENO_TESTING_REVOCATION_KEYS_FILE` points.
const b64 = (bytes: Uint8Array) => btoa(String.fromCharCode(...bytes));
const pair = await crypto.subtle.generateKey(
  { name: "ECDSA", namedCurve: "P-256" },
  true,
  ["sign", "verify"],
);
const spki = new Uint8Array(
  await crypto.subtle.exportKey("spki", pair.publicKey),
);
const now = Date.now();
const signed = JSON.stringify({
  schema: 1,
  issuedAt: new Date(now - 60_000).toISOString(),
  expiresAt: new Date(now + 86_400_000).toISOString(),
  sequence: Math.floor(now / 1000) - 60,
  revoked: [{
    component: "cli",
    version: Deno.version.deno,
    targets: [Deno.build.target],
    reason: "a test revocation",
    advisory: "https://example.com/advisory",
    fixedIn: "3.0.0-denext.999",
  }],
});
const signature = new Uint8Array(
  await crypto.subtle.sign(
    { name: "ECDSA", hash: "SHA-256" },
    pair.privateKey,
    new TextEncoder().encode("denext-runtime-revocations-v1\n" + signed),
  ),
);
Deno.mkdirSync("deno_dir/denext-deno", { recursive: true });
Deno.writeTextFileSync(
  "deno_dir/denext-deno/revocations.json",
  JSON.stringify({ signed, signature: b64(signature) }),
);
Deno.writeTextFileSync("keys.json", JSON.stringify([b64(spki)]));
