// A timer every 50 ms for 1.5 s keeps waking the event loop, so it is never
// idle for the 500 ms delay and no trim may run.
let ticks = 0;
await new Promise((resolve) => {
  const id = setInterval(() => {
    if (++ticks === 30) {
      clearInterval(id);
      resolve();
    }
  }, 50);
});
console.log("done");
