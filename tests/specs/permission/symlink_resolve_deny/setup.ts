const cwd = Deno.cwd();
Deno.mkdirSync("allowed");
Deno.mkdirSync("denied");
Deno.writeTextFileSync("denied/secret.txt", "SECRET");
Deno.writeTextFileSync("denied/secret.env", "DENIED_SECRET=1\n");
Deno.writeTextFileSync("denied/remove.txt", "KEEP");
Deno.writeTextFileSync("denied/rename.txt", "KEEP");
Deno.mkdirSync("denied/tree");
Deno.writeTextFileSync("denied/tree/file.txt", "KEEP");
Deno.writeTextFileSync("allowed/src.txt", "SOURCE");
Deno.writeTextFileSync("allowed/src_rename.txt", "SOURCE");
// a dangling symlink into the denied directory
Deno.symlinkSync(`${cwd}/denied/new.txt`, "allowed/dangling");
// a relative dangling chain: chain1 -> chain2 -> ../denied/chained.txt
Deno.symlinkSync("chain2", "allowed/chain1");
Deno.symlinkSync("../denied/chained.txt", "allowed/chain2");
// a dangling symlink whose target's parent is reached through another link
Deno.symlinkSync("dir/via_dir.txt", "allowed/via_dir");
// symlinks to an existing denied file and to the denied directory
Deno.symlinkSync(`${cwd}/denied/secret.txt`, "allowed/existing");
Deno.symlinkSync("../denied/secret.env", "allowed/env");
Deno.symlinkSync(`${cwd}/denied`, "allowed/dir");
// a symlink cycle
Deno.symlinkSync("loop2", "allowed/loop1");
Deno.symlinkSync("loop1", "allowed/loop2");
console.log("setup done");
