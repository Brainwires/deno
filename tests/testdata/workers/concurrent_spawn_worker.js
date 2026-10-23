// Copyright 2018-2026 the Deno authors. MIT license.

// Spawns child processes for `commandConcurrentSpawnsDoNotLeakPipes` in
// tests/unit/command_test.ts (Windows only).
//
// "short": runs `cmd /c echo` `count` times through `Deno.Command` or
// `node:child_process` and reports the slowest run.
// "long": keeps spawning long-lived children until told to stop, then kills
// them. A long-lived child that inherits a short run's stdout write end keeps
// that pipe open, and the short run's output never ends.
import { execFile } from "node:child_process";

function nodeEcho() {
  return new Promise((resolve, reject) => {
    execFile("cmd", ["/c", "echo", "hi"], (err, stdout) => {
      if (err) reject(err);
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
  const echo = api === "node" ? nodeEcho : denoEcho;
  let max = 0;
  for (let i = 0; i < count; i++) {
    const start = performance.now();
    const text = await echo();
    max = Math.max(max, performance.now() - start);
    if (!text.includes("hi")) throw new Error(`unexpected output: ${text}`);
  }
  postMessage({ max });
}

async function runLong({ max, seconds }) {
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
      new Deno.Command("ping", {
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
