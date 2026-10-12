it("beta uses its own config", (ctx) => {
  expect(ctx.task.timeout).toBe(5000);
});
