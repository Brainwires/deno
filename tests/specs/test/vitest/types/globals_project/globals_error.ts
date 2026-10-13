// A wrongly typed use of a global is a type error.
/// <reference types="vitest/globals" />

it("is wrongly typed", () => {
  vi.fn((n: number) => n).mockReturnValue("one");
});
