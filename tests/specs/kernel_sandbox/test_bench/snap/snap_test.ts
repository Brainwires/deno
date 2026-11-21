// Copyright 2018-2026 the Deno authors. MIT license.
// The default snapshot location needs no grant: the confined image reads it.
Deno.test("snap", async (t) => {
  await t.assertSnapshot({ value: 1 });
});
