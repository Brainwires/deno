// What a lifecycle script can reach through `node` (a Deno process). Writes
// one line per check to results.txt in this package's folder.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const initCwd = process.env.INIT_CWD;
const results = [];
const record = (line) => results.push("node: " + line);
function attempt(name, fn) {
  try {
    fn();
    record(`${name}: allowed`);
  } catch {
    record(`${name}: denied`);
  }
}

record(`GITHUB_TOKEN: ${process.env.GITHUB_TOKEN ? "visible" : "missing"}`);
record(`NPM_TOKEN: ${process.env.NPM_TOKEN ? "visible" : "missing"}`);
record(
  `npm auth config: ${
    Object.keys(process.env).some((k) => /authtoken/i.test(k))
      ? "visible"
      : "missing"
  }`,
);
record(`JAIL_PASSED_VAR: ${process.env.JAIL_PASSED_VAR ?? "missing"}`);
record(`home: ${os.homedir().includes(".jail") ? "scratch" : "real"}`);
attempt("write own dir", () =>
  fs.writeFileSync(path.join(__dirname, "own.txt"), "x"));
attempt("write TMPDIR", () => {
  const file = path.join(os.tmpdir(), `jail-probe-${process.pid}.txt`);
  fs.writeFileSync(file, "x");
  fs.unlinkSync(file);
});
const systemTemp = process.platform === "win32"
  ? path.join(process.env.SystemRoot ?? "C:\\Windows", "Temp")
  : "/tmp";
attempt("write system temp", () => {
  const file = path.join(systemTemp, `jail-probe-${process.pid}.txt`);
  fs.writeFileSync(file, "x");
  fs.unlinkSync(file);
});
attempt("write project", () =>
  fs.writeFileSync(path.join(initCwd, "pwned-node.txt"), "x"));
attempt("write via workspace link", () =>
  fs.writeFileSync(
    path.join(initCwd, "node_modules", "member", "pwned-node-link.txt"),
    "x",
  ));
attempt("create symlink", () =>
  fs.symlinkSync(initCwd, path.join(__dirname, "node-link")));
attempt("read .env", () => fs.readFileSync(path.join(initCwd, ".env")));
attempt("read project package.json", () =>
  fs.readFileSync(path.join(initCwd, "package.json")));

if (process.platform === "linux") {
  // the installer's environment through /proc (its GITHUB_TOKEN)
  let pid = process.pid;
  let leaked = false;
  for (let i = 0; i < 8; i++) {
    let status;
    try {
      status = fs.readFileSync(`/proc/${pid}/status`, "utf8");
    } catch {
      break;
    }
    const match = status.match(/PPid:\s+(\d+)/);
    if (!match) break;
    pid = Number(match[1]);
    if (pid <= 1) break;
    try {
      if (fs.readFileSync(`/proc/${pid}/environ`, "utf8").includes("GITHUB_TOKEN=")) {
        leaked = true;
      }
    } catch {
      // not readable
    }
  }
  let selfReadable = false;
  try {
    fs.readFileSync("/proc/self/environ");
    selfReadable = true;
  } catch {
    // not readable
  }
  record(`own environ: ${selfReadable ? "readable" : "unreadable"}`);
  record(`ancestor environ: ${leaked ? "leaked" : "hidden"}`);
} else {
  record("own environ: n/a");
  record("ancestor environ: n/a");
}

(async () => {
  try {
    await fetch("http://localhost:4545/");
    record("fetch: allowed");
  } catch {
    record("fetch: denied");
  }
  fs.appendFileSync(
    path.join(__dirname, "results.txt"),
    results.join("\n") + "\n",
  );
})();
