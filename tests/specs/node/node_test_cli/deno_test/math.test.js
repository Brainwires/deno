// A test file written for `deno test`: under `node --test` its tests would
// not run, so the file fails instead of passing with none.
Deno.test("adds", () => {
  if (1 + 1 !== 2) throw new Error("1 + 1 is not 2");
});
