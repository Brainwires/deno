// The runtime's own variables must not have been imported from the env
// file...
for (
  const name of [
    "DENO_AUDIT_PERMISSIONS",
    "DENO_COVERAGE_DIR",
    "DENO_UNSTABLE_CRON_SOCK",
    "DENEXT_DEV_TOKEN",
    "DENO_INTERNAL_LIFECYCLE_JAIL",
    "DENO_PERMISSION_BROKER_PATH",
  ]
) {
  console.log(`${name}: ${Deno.env.get(name)}`);
}
// ...and nothing was written for them...
for (const path of ["audit.jsonl", "cov"]) {
  let exists = true;
  try {
    Deno.statSync(path);
  } catch {
    exists = false;
  }
  console.log(`${path} exists: ${exists}`);
}
// ...but ordinary variables from the same file still are.
console.log("APP_SETTING:", Deno.env.get("APP_SETTING"));
