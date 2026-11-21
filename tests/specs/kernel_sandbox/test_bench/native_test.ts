// Copyright 2018-2026 the Deno authors. MIT license.
// Inside `deno test` the kernel holds native code to the top-level grant.
import { openRead } from "./native.ts";

Deno.test("native reads", () => {
  openRead("read inside", "inside/a.txt");
  openRead("read outside", "outside.txt");
});
