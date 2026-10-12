// Matched by both projects: Jest runs it once in each.
test("runs in each project", () => {
  expect(["alpha", "beta"]).toContain(
    globalThis.__vitest_worker__.config.name,
  );
});
