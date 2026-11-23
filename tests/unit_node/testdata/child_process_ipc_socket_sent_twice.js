// Reports, for each message the parent sends, whether a handle came with it,
// and answers on the socket it got. See the test of the same name in
// child_process_test.ts.
process.on("message", (message, handle) => {
  process.send({ message, handle: handle !== undefined });
  if (handle) {
    handle.end("from child");
  }
  if (message === "second") {
    process.disconnect();
  }
});
process.send("ready");
