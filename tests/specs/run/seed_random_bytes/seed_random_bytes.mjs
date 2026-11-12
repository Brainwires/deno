import { createHash, randomBytes, randomFillSync } from "node:crypto";

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

console.log(
  "getRandomValues(16)",
  Buffer.from(crypto.getRandomValues(new Uint8Array(16))).toString("hex"),
);
console.log("randomBytes(5)", randomBytes(5).toString("hex"));
console.log("randomBytes(65536)", sha256(randomBytes(65536)));
console.log("randomBytes(70000)", sha256(randomBytes(70000)));
console.log("randomBytes(200003)", sha256(randomBytes(200003)));
console.log(
  "randomFillSync(9)",
  randomFillSync(Buffer.alloc(9)).toString("hex"),
);
console.log(
  "getRandomValues(65536)",
  sha256(crypto.getRandomValues(new Uint8Array(65536))),
);
const viaCallback = await new Promise((resolve, reject) => {
  randomBytes(100000, (err, buf) => err ? reject(err) : resolve(buf));
});
console.log("randomBytes(100000, cb)", sha256(viaCallback));
console.log("randomUUID", crypto.randomUUID());
