// "fixed": allocate about 16 MB (4 arrays of 1Mi small integers; 32 MB
// without pointer compression) and report it. "grow_on_message": allocate
// until the heap limit stops the worker.
self.onmessage = (e: MessageEvent<string>) => {
  if (e.data === "fixed") {
    const chunks = [];
    for (let i = 0; i < 4; i++) chunks.push(new Array(1024 * 1024).fill(0));
    self.postMessage(`allocated ${chunks.length} chunks`);
  } else {
    const chunks = [];
    while (true) chunks.push(new Array(1024 * 1024).fill(0));
  }
};
