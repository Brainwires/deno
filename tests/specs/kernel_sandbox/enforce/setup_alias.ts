// Copyright 2018-2026 the Deno authors. MIT license.
// A second name for inside/ (a symlink), and a folder beneath it.
Deno.symlinkSync(Deno.realPathSync("inside"), "alias");
Deno.mkdirSync("inside/sub");
Deno.writeTextFileSync("inside/sub/b.txt", "b");
