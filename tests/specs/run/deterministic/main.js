// The same values under `--deterministic` as under `--seed=1`, whatever the
// host's time zone and locale. (The cryptographic sources: crypto.js.)
console.log(Math.random(), Math.random());
console.log(Intl.DateTimeFormat().resolvedOptions().timeZone);
console.log(navigator.language);
console.log(new Date(0).toString());
console.log((1234567.891).toLocaleString());
