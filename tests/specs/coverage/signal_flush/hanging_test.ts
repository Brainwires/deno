function ready() {
  console.log("ready");
}

Deno.test("waits for the signal", async () => {
  ready();
  // Never settles: the run ends by the signal.
  await new Promise(() => {
    setInterval(() => {}, 1000);
  });
});
