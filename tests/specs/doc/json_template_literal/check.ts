// `deno doc --json` with colors forced on: no string in the JSON may hold an
// ESC byte (serde_json writes one as `\u001b`, so the parsed values are checked).
const { success, stdout } = await new Deno.Command(Deno.execPath(), {
  args: ["doc", "--json", "mod.ts"],
  env: { FORCE_COLOR: "1" },
}).output();
if (!success) throw new Error("deno doc --json failed");
const text = new TextDecoder().decode(stdout);
const json = JSON.parse(text);
const escapes: string[] = [];
JSON.stringify(json, (_key, value) => {
  if (typeof value === "string" && value.includes("\x1b")) escapes.push(value);
  return value;
});
if (escapes.length > 0 || text.includes("\x1b")) {
  throw new Error(
    `ESC byte in deno doc --json output: ${JSON.stringify(escapes)}`,
  );
}
type DocSymbol = {
  name: string;
  declarations: { def: { tsType: { repr: string } } }[];
};
for (const doc of Object.values(json.nodes) as { symbols: DocSymbol[] }[]) {
  for (const symbol of doc.symbols) {
    console.log(`${symbol.name}: ${symbol.declarations[0].def.tsType.repr}`);
  }
}
console.log("no ESC byte");
