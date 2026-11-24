// Changes the signed payload so the signature no longer covers it.
const path = "deno_dir/denext-deno/revocations.json";
const envelope = JSON.parse(Deno.readTextFileSync(path));
envelope.signed = envelope.signed.replace("a test revocation", "tampered");
Deno.writeTextFileSync(path, JSON.stringify(envelope));
