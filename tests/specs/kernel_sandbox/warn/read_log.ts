// the log's records, without what differs between machines (the rules an
// OS can't hold are that machine's own)
const lines = Deno.readTextFileSync("warn.jsonl").trim().split("\n")
  .map((l) => JSON.parse(l));
for (const r of lines) {
  if (r.type === "run") {
    console.log("run", r.v, typeof r.pid, r.observer, typeof r.policy);
  } else if (r.class !== "startup") {
    console.log(
      r.type,
      r.v,
      r.class,
      r.source,
      r.target,
      r.grant ?? "no grant",
    );
  }
}
