// Copyright 2018-2026 the Deno authors. MIT license.
// A nested deno whose child fails: it runs inside the sandbox it
// inherited, so it prints the hint too.
new Deno.Command("sh", { args: ["-c", "exit 3"] }).outputSync();
