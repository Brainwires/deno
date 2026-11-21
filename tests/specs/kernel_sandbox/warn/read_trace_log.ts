// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The warn log of a traced run: the run's own record says a supervisor
// observes it, and the supervisor's record of the child's read is there.
const records = Deno.readTextFileSync("trace.jsonl").trim().split("\n")
  .map((line) => JSON.parse(line));
const run = records.find((r) =>
  r.type === "run" && r.observer === "supervisor-trace"
);
console.log(`a run observed by the supervisor's trace: ${run !== undefined}`);
const read = records.find((r) =>
  r.type === "wouldDeny" && r.class === "read" &&
  String(r.target).endsWith("outside.txt")
);
console.log(`the child's read: ${read?.source} ${read?.op} by ${read?.process?.name}`);
console.log(`the grant: ${read?.grant?.flag}=…${String(read?.grant?.value).endsWith("outside.txt")}`);
