// a prebuilt-binary download from the host package.json declares, and one
// from a host it doesn't
const fs = require("node:fs");
const path = require("node:path");
async function attempt(url) {
  try {
    // the jail's proxy answers a refused host with 403 (DENEXT.md item 124)
    const response = await fetch(url);
    return response.status === 403 ? "denied" : "allowed";
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
