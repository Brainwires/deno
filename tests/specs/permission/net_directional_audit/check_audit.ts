// The net records of main.ts, with the listener's port left out.
const records = Deno.readTextFileSync("audit.jsonl")
  .split("\n")
  .filter((line) => line.length > 0)
  .map((line) => JSON.parse(line))
  .filter((record) => String(record.permission).startsWith("net"))
  .map((record) =>
    `${record.permission} ${String(record.value).replace(/:\d+$/, ":<port>")}`
  );
console.log([...new Set(records)].join("\n"));
