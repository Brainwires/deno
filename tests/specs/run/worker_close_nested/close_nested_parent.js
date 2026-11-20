// Copyright 2018-2026 the Deno authors. MIT license.

console.log("Starting the parent worker");

const child = new Worker(
  import.meta.resolve("./close_nested_child.js"),
  { type: "module" },
);
// Tell the main thread once the child is running.
child.onmessage = () => self.postMessage("child started");

self.addEventListener("message", () => {
  console.log("Closing");
  self.close();
});
