// What electron's postinstall does when extract-zip unpacks the macOS app
// bundle: relative symlinks inside the package's own folder, some created
// before their target exists. And the links that would leave the folder.
// One line per check in results.txt.
const fs = require("node:fs");
const path = require("node:path");

const initCwd = process.env.INIT_CWD;
const framework = path.join(
  __dirname,
  "dist",
  "Electron.app",
  "Contents",
  "Frameworks",
  "Electron Framework.framework",
);
const results = [];
function attempt(name, fn) {
  try {
    fn();
    results.push(`node: ${name}: allowed`);
  } catch {
    results.push(`node: ${name}: denied`);
  }
}

async function main() {
  fs.mkdirSync(path.join(framework, "Versions", "A", "Resources"), {
    recursive: true,
  });
  fs.writeFileSync(
    path.join(framework, "Versions", "A", "Electron Framework"),
    "binary",
  );
  attempt("Versions/Current -> A", () =>
    fs.symlinkSync("A", path.join(framework, "Versions", "Current")));
  attempt("binary -> Versions/Current/...", () =>
    fs.symlinkSync(
      "Versions/Current/Electron Framework",
      path.join(framework, "Electron Framework"),
    ));
  try {
    await fs.promises.symlink(
      "Versions/Current/Resources",
      path.join(framework, "Resources"),
    );
    results.push("node: async Resources -> Versions/Current/Resources: allowed");
  } catch {
    results.push("node: async Resources -> Versions/Current/Resources: denied");
  }
  // extract-zip creates a link before the entry it points at
  attempt("dangling Libraries -> Versions/Current/Libraries", () =>
    fs.symlinkSync(
      "Versions/Current/Libraries",
      path.join(framework, "Libraries"),
    ));
  fs.mkdirSync(path.join(framework, "Versions", "A", "Libraries"));
  let read;
  try {
    read = fs.readFileSync(path.join(framework, "Electron Framework"), "utf8");
  } catch {
    read = "unreadable";
  }
  results.push(`node: read through the links: ${read}`);

  attempt("target outside (absolute)", () =>
    fs.symlinkSync(initCwd, path.join(__dirname, "out-abs")));
  attempt("target outside (relative)", () =>
    fs.symlinkSync("../../..", path.join(__dirname, "out-rel")));
  attempt("link outside the package", () =>
    fs.symlinkSync(
      path.join(__dirname, "index.js"),
      path.join(initCwd, "pwned-link"),
    ));
  // `shout` (made by `ln -s`) leads out of the folder; `..` after it is
  // resolved by the kernel from there, not folded away
  attempt("target through a link, then ..", () =>
    fs.symlinkSync("shout/../escaped", path.join(__dirname, "out-dotdot")));
  fs.writeFileSync(
    path.join(__dirname, "results.txt"),
    results.join("\n") + "\n",
  );
}

main();
