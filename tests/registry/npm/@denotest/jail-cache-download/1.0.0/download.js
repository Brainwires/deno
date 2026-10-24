// a browser download as puppeteer's install script does it: fetched from a
// download host and saved in the cache directory PUPPETEER_CACHE_DIR names
// (else ~/.cache/puppeteer). A relative PUPPETEER_CACHE_DIR is taken from the
// project directory (INIT_CWD): a spec test can't name its temp directory.
const fs = require("node:fs");
const os = require("node:os");
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
  const listed = await attempt("http://localhost:5548/");
  const other = await attempt("http://localhost:4545/");
  const cacheDir = process.env.PUPPETEER_CACHE_DIR
    ? path.resolve(process.env.INIT_CWD, process.env.PUPPETEER_CACHE_DIR)
    : path.join(os.homedir(), ".cache", "puppeteer");
  let cache;
  try {
    fs.mkdirSync(path.join(cacheDir, "chrome"), { recursive: true });
    fs.writeFileSync(path.join(cacheDir, "chrome", "browser.txt"), "browser\n");
    cache = "saved";
  } catch (err) {
    cache = `denied (${err.code ?? err.name})`;
  }
  fs.writeFileSync(
    path.join(__dirname, "results.txt"),
    `listed host: ${listed}\nother host: ${other}\ncache: ${cache}\n`,
  );
})();
