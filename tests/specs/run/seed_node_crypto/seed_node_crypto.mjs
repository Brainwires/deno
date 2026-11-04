import { randomFill, randomFillSync, randomInt } from "node:crypto";

const ints = [];
for (let i = 0; i < 5; ++i) {
  ints.push(randomInt(1_000_000));
}
console.log("randomInt", ints.join(" "));
console.log("randomInt(min, max)", randomInt(-50, 50));

await new Promise((resolve, reject) => {
  randomInt(1_000_000, (err, n) => {
    if (err) reject(err);
    console.log("randomInt(cb)", n);
    resolve();
  });
});

console.log("randomFillSync", randomFillSync(new Uint8Array(16)).join(" "));

const filled = await new Promise((resolve, reject) => {
  randomFill(new Uint8Array(16), (err, buf) => {
    if (err) reject(err);
    else resolve(buf);
  });
});
console.log("randomFill", filled.join(" "));

// Two async fills started together take the seeded bytes in call order.
const [a, b] = await Promise.all(
  [1, 2].map(() =>
    new Promise((resolve, reject) => {
      randomFill(new Uint8Array(4), (err, buf) => {
        if (err) reject(err);
        else resolve(buf);
      });
    })
  ),
);
console.log("randomFill (ordered)", a.join(" "), "|", b.join(" "));

const arr = new Uint8Array(8);
crypto.getRandomValues(arr);
console.log("getRandomValues", arr.join(" "));
