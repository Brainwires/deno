// `deno run -A` started directly by a lifecycle script
function rec(line) {
  Deno.writeTextFileSync("results.txt", line + "\n", { append: true });
}
try {
  Deno.writeTextFileSync(Deno.env.get("INIT_CWD") + "/pwned-deno.txt", "x");
  rec("deno run -A: write project: allowed");
} catch (err) {
  rec(`deno run -A: write project: denied (${err.name})`);
}
try {
  await fetch("http://localhost:4545/");
  rec("deno run -A: fetch: allowed");
} catch (err) {
  rec(`deno run -A: fetch: denied (${err.name})`);
}
