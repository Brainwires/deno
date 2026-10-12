const add = jest.fn((a: number, b: number) => a + b);

test("Jest's default testMatch picks *.spec.ts", () => {
  expect(add(1, 2)).toBe(3);
  expect(add).toHaveBeenCalledTimes(1);
});

test("clearMocks clears calls before each test", () => {
  expect(add).not.toHaveBeenCalled();
});
