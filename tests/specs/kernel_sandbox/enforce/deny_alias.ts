// Copyright 2018-2026 the Deno authors. MIT license.
// A read deny spelled through another path than its allow holds against
// native code, and so does an allow that lies inside a deny.
import { openRead } from "./native.ts";

const [mode] = Deno.args;
if (mode === "alias") {
  openRead("read through the allow's name", "alias/a.txt");
  openRead("read denied, deny's name", "inside/secret.txt");
  openRead("read denied, allow's name", "alias/secret.txt");
} else {
  openRead("read inside the denied folder", "inside/sub/b.txt");
  openRead("read through the other name", "alias/sub/b.txt");
}
