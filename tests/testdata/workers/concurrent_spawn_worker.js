// Copyright 2018-2026 the Deno authors. MIT license.

// Spawns child processes for `commandConcurrentSpawnsDoNotLeakPipes` in
// tests/unit/command_test.ts (Windows only).
//
// "short": runs `cmd /c echo` `count` times through `Deno.Command` or
// `node:child_process` (`api: "node-extra"`: with two extra `stdio` pipes,
// fds 3 and 4, which `close` waits for too) and reports the slowest run.
// "long": keeps spawning long-lived children (`api: "node-extra"`: through
// `node:child_process` with extra pipes of their own) until told to stop,
// then kills them. A long-lived child that inherits a short run's stdout
// write end keeps that pipe open, and the short run's output never ends.
// Also used by `commandConcurrentExtraStdioSpawnsDoNotLeakPipes`.
import { execFile, spawn } from "node:child_process";

function nodeEcho() {
  return new Promise((resolve, reject) => {
    execFile("cmd", ["/c", "echo", "hi"], (err, stdout) => {
      if (err) reject(err);
      else resolve(stdout);
    });
  });
}

function nodeExtraEcho() {
  return new Promise((resolve, reject) => {
    const child = spawn("cmd", ["/c", "echo", "hi"], {
      stdio: ["ignore", "pipe", "ignore", "pipe", "pipe"],
    });
    let stdout = "";
    child.on("error", reject);
    child.stdout.on("data", (data) => stdout += data);
    for (const stream of child.stdio.slice(3)) stream?.resume();
    // `close` comes once the child exited AND stdout and both extra pipes
    // reached end-of-file.
    child.on("close", (code) => {
      if (code !== 0) reject(new Error(`cmd exited with ${code}`));
      else resolve(stdout);
    });
  });
}

async function denoEcho() {
  const out = await new Deno.Command("cmd", { args: ["/c", "echo", "hi"] })
    .output();
  if (!out.success) throw new Error(`cmd exited with ${out.code}`);
  return new TextDecoder().decode(out.stdout);
}

async function runShort({ count, api }) {
  const echo = api === "node"
    ? nodeEcho
    : api === "node-extra"
    ? nodeExtraEcho
    : denoEcho;
  let max = 0;
  for (let i = 0; i < count; i++) {
    const start = performance.now();
    const text = await echo();
    max = Math.max(max, performance.now() - start);
    if (!text.includes("hi")) throw new Error(`unexpected output: ${text}`);
  }
  postMessage({ max });
}

function nodeExtraPing(seconds) {
  const child = spawn("ping", ["-n", String(seconds), "127.0.0.1"], {
    stdio: ["ignore", "ignore", "ignore", "pipe"],
  });
  child.stdio[3]?.resume();
  const exited = new Promise((resolve) => child.on("exit", resolve));
  return {
    kill: () => child.kill(),
    status: exited,
  };
}

async function runLong({ max, seconds, api }) {
  const children = [];
  let stopping = false;
  onmessage = async () => {
    stopping = true;
    for (const child of children) {
      try {
        child.kill();
      } catch {
        // already exited
      }
      await child.status;
    }
    postMessage({ stopped: children.length });
  };
  while (!stopping && children.length < max) {
    children.push(
      api === "node-extra" ? nodeExtraPing(seconds) : new Deno.Command("ping", {
        args: ["-n", String(seconds), "127.0.0.1"],
        stdin: "null",
        stdout: "null",
        stderr: "null",
      }).spawn(),
    );
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

onmessage = (e) => {
  const run = e.data.kind === "short" ? runShort : runLong;
  run(e.data).catch((err) => {
    postMessage({ error: String(err?.stack ?? err) });
  });
};
