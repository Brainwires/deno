// The unresolvable optional dep is absent from the lockfile; its sibling
// is recorded.
const lock = Deno.readTextFileSync("deno.lock");
console.log("linux-x64:", lock.includes("napi-optional-platform-linux-x64"));
console.log(
  "android-arm64:",
  lock.includes("napi-optional-platform-android-arm64"),
);
