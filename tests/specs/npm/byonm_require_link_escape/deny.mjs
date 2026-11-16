// The same file as a JSON module.
const file = ["other", "node_modules", "x", "s.json"].join("/");
try {
  const mod = await import(new URL(file, import.meta.url).href, {
    with: { type: "json" },
  });
  console.log(`json import: ${mod.default.secret}`);
} catch (error) {
  // An import() refusal is a TypeError naming the read, as for any file.
  console.log(`json import: ${error.name}: ${error.message.split(",")[0]}`);
}
