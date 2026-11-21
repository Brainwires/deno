// Copyright 2018-2026 the Deno authors. MIT license.
// Inside `deno bench` the kernel holds native code to the top-level grant.
import { openRead } from "./native.ts";

openRead("read inside", "inside/a.txt");
openRead("read outside", "outside.txt");

Deno.bench("noop", () => {});
