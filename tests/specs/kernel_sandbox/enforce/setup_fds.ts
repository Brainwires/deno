// Copyright 2018-2026 the Deno authors. MIT license.
// Builds the descriptor lister into lib/ (the confined run's read scope).
const isMac = Deno.build.os === "darwin";
Deno.mkdirSync("lib", { recursive: true });
const out = `lib/libfds.${isMac ? "dylib" : "so"}`;
const args = isMac
  ? ["-dynamiclib", "-o", out, "fds.c"]
  : ["-shared", "-fPIC", "-o", out, "fds.c"];
const o = new Deno.Command("cc", { args }).outputSync();
if (!o.success) throw new Error(new TextDecoder().decode(o.stderr));
