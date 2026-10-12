((globalThis as Record<string, unknown>).setupOrder as string[]).push(
  "setupFilesAfterEnv",
);
expect.extend({
  toBeEven(received: number) {
    return {
      pass: received % 2 === 0,
      message: () => `expected ${received} to be even`,
    };
  },
});
