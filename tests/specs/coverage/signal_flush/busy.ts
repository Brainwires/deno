// Stuck in synchronous JavaScript: its coverage can't be flushed, so a signal
// waits for the flush until its timeout, unless the signal comes again.
console.log("ready");
const end = Date.now() + 60_000;
while (Date.now() < end) {
  // busy
}
