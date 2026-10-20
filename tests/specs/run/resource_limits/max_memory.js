// Allocates until the heap reaches --max-memory. Catching errors doesn't help:
// the limit terminates execution, which no `catch` sees.
console.log("start");
const arrays = [];
try {
  while (true) {
    arrays.push(new Array(1_000_000).fill(Math.random()));
  }
} catch (e) {
  console.log("caught", e);
} finally {
  console.log("finally");
}
console.log("after");
