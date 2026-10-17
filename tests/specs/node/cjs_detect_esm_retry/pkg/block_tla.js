// An await in a nested top-level block makes this an ES module.
if (globalThis.Deno) {
  await Promise.resolve();
  console.log("esm", typeof __dirname);
}
