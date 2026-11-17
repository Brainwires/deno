import { spawn, spawnSync } from "node:child_process";

function outcome(err) {
  return err.name === "NotCapable" ? "denied" : `error (${err.name})`;
}

function attempt(name, argv0) {
  try {
    const r = spawnSync("/bin/sh", ["-c", "echo ran"], {
      argv0,
      encoding: "utf8",
    });
    console.log(`${name}: ${r.error ? outcome(r.error) : r.stdout.trim()}`);
  } catch (err) {
    console.log(`${name}: ${outcome(err)}`);
  }
}

attempt("own path", "/bin/sh");
attempt("own name", "sh");
attempt("another name", "ls");
attempt("another path", "/bin/ls");

await new Promise((resolve) => {
  let out = "";
  try {
    const child = spawn("/bin/sh", ["-c", "echo ran"], { argv0: "ls" });
    child.stdout.on("data", (chunk) => out += chunk);
    child.on("error", (err) => {
      console.log(`async another name: ${outcome(err)}`);
      resolve();
    });
    child.on("close", (code) => {
      if (code !== null && out) {
        console.log(`async another name: ${out.trim()}`);
      }
      resolve();
    });
  } catch (err) {
    console.log(`async another name: ${outcome(err)}`);
    resolve();
  }
});
