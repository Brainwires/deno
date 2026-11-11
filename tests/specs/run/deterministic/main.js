// The same values under `--deterministic` as under `--seed=1`, whatever the
// host's time zone and locale.
console.log(Math.random(), Math.random());
console.log(crypto.getRandomValues(new Uint8Array(8)).join(","));
console.log(crypto.randomUUID());
console.log(Intl.DateTimeFormat().resolvedOptions().timeZone);
console.log(navigator.language);
console.log(new Date(0).toString());
console.log((1234567.891).toLocaleString());
