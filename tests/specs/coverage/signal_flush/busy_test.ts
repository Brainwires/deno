Deno.test("is stuck in synchronous code", () => {
  console.log("ready");
  const end = Date.now() + 60_000;
  while (Date.now() < end) {
    // busy
  }
});
