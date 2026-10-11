test("alpha runs", () => {
  expect(globalThis.__vitest_worker__.config.name).toBe("alpha");
});
