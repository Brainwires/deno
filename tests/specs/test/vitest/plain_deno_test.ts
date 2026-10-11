// A regular Deno test file: untouched by the Vitest layer.
Deno.test("plain", () => {
  if (typeof (globalThis as Record<string, unknown>).__vitest_worker__ !== "undefined") {
    throw new Error("the Vitest runtime must not load for plain tests");
  }
});
