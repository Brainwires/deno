// Copyright 2018-2026 the Deno authors. MIT license.

// Test that closing a worker which has living child workers will automatically
// close the children.

console.log("Starting the main thread");

const worker = new Worker(
  import.meta.resolve("./close_nested_parent.js"),
  { type: "module" },
);

// Close the parent once the child has started (a fixed delay raced a slow child
// start on a busy machine).
worker.onmessage = () => {
  console.log("About to close");
  worker.postMessage({});

  // Keep the process running for another two seconds, to make sure there's no
  // output from the child worker.
  setTimeout(() => {}, 2000);
};
