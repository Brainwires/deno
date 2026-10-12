/**
 * @jest-environment jsdom
 */
it("uses the docblock's test environment", () => {
  expect(document.title).toBe("stub document");
  // like Jest's DOM environments
  expect(typeof setImmediate).toBe("undefined");
  // deno-lint-ignore no-explicit-any
  const win = window as any;
  expect("Deno" in win).toBe(false);
  win.Deno = "1";
  expect("Deno" in win).toBe(true);
  delete win.Deno;
  expect("Deno" in win).toBe(false);
});
