// a prebuilt-binary download from the host package.json declares, and one
// from a host it doesn't
const fs = require("node:fs");
const path = require("node:path");
async function attempt(url) {
  try {
    await fetch(url);
    return "allowed";
  } catch {
    return "denied";
  }
}
(async () => {
  const declared = await attempt("http://localhost:4545/");
  const other = await attempt("http://localhost:4546/");
  fs.writeFileSync(
    path.join(__dirname, "results.txt"),
    `declared host: ${declared}\nother host: ${other}\n`,
  );
})();
