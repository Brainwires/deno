// A node:crypto job started in a timer callback completes through the event
// loop, like a job on libuv's thread pool: its callback runs after every
// timer that is due in the same timers phase, never during the call, from
// `process.nextTick`, or in the microtask checkpoint between two timer lists.
//
// The first four results are ready when the call returns (random bytes drawn
// on the spot, nothing to derive, a secret computed during the call): the
// cases that were delivered early.
const crypto = require("node:crypto");

const alice = crypto.generateKeyPairSync("x25519");
const bob = crypto.generateKeyPairSync("x25519");

const calls = {
  "crypto.randomBytes": (cb) => crypto.randomBytes(8, cb),
  "crypto.randomFill": (cb) => crypto.randomFill(Buffer.alloc(8), cb),
  "crypto.scrypt (no key)": (cb) => crypto.scrypt("password", "salt", 0, cb),
  "crypto.diffieHellman": (cb) =>
    crypto.diffieHellman(
      { privateKey: alice.privateKey, publicKey: bob.publicKey },
      cb,
    ),
  "crypto.pbkdf2": (cb) =>
    crypto.pbkdf2("password", "salt", 1, 8, "sha256", cb),
  "crypto.scrypt": (cb) => crypto.scrypt("password", "salt", 8, { N: 2 }, cb),
  "crypto.hkdf": (cb) => crypto.hkdf("sha256", "key", "salt", "info", 8, cb),
  "crypto.generateKey": (cb) => crypto.generateKey("hmac", { length: 64 }, cb),
  "crypto.generateKeyPair": (cb) => crypto.generateKeyPair("ed25519", cb),
  "crypto.checkPrime": (cb) => crypto.checkPrime(7n, cb),
  "crypto.generatePrime": (cb) => crypto.generatePrime(16, cb),
};

function busy(ms) {
  const end = performance.now() + ms;
  while (performance.now() < end);
}

function check(name) {
  return new Promise((resolve) => {
    let secondTimerRan = false;
    let returned = false;
    let completed = false;
    setTimeout(() => {
      calls[name]((err) => {
        if (completed) return;
        completed = true;
        const when = !returned
          ? "DURING the call"
          : secondTimerRan
          ? "after the second timer"
          : "BEFORE the second timer";
        resolve(`${name}: ${when}${err ? ` (${err.code ?? err.name})` : ""}`);
      });
      returned = true;
    }, 1);
    // Another duration, so another timer list: the microtask checkpoint
    // between the two lists is where an early result was delivered.
    setTimeout(() => {
      secondTimerRan = true;
    }, 2);
    // Both timers are due when the timers phase starts.
    busy(10);
  });
}

(async () => {
  const only = process.argv.slice(2);
  for (const name of only.length ? only : Object.keys(calls)) {
    console.log(await check(name));
  }
})();
