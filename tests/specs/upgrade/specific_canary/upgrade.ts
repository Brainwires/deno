// Copyright 2018-2026 the Deno authors. MIT license.
// Fetch the latest canary hash for this target and upgrade to it. The
// target-agnostic `canary-latest.txt` can name a commit whose archive for
// this target isn't published (yet, or ever), so read the per-target
// pointer, the one `deno upgrade --canary` itself reads.
const url = `https://dl.deno.land/canary-${Deno.build.target}-latest.txt`;
const hash = (await (await fetch(url)).text()).trim();
const { code } = await new Deno.Command("./deno_copy", {
  args: ["upgrade", "--force", "--canary", "--version", hash],
  stdout: "inherit",
  stderr: "inherit",
}).output();
Deno.exit(code);
