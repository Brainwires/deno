// @vitest-environment jsdom
describe("jsdom environment", () => {
  it("installs the window", () => {
    expect(globalThis.window).toBe(globalThis);
    expect(document.title).toBe("stub document");
    expect((globalThis as Record<string, unknown>).jsdom).toBeDefined();
    expect(import.meta.env.SSR).toBe(false);
  });
});
