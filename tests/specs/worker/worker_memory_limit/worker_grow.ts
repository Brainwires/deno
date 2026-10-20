// Allocates until the heap limit stops the worker, while the module evaluates.
const chunks = [];
while (true) chunks.push(new Array(1024 * 1024).fill(0));
