// A remote module only a computed dynamic import names isn't in the static
// graph the first image fetched, so the confined run can't fetch it: on
// Linux it reads remote modules from the cache only (denext.30 stage-r4:
// the documented "a dynamic remote import() fails" behaviour). The query
// string keeps the URL out of any earlier cache.
const url = "http://localhost:4545/subdir/mod2.ts?kernel_sandbox_dyn=" +
  Date.now();
try {
  await import(url);
  console.log("imported");
} catch (err) {
  const message = err instanceof Error ? err.message : String(err);
  console.log(
    message.includes("not found in cache") ? "not found in cache" : message,
  );
}
