// Spins (so the profile has samples), then signals itself.
const signal = process.argv[2];
const end = Date.now() + 50;
while (Date.now() < end);
if (process.argv[3] === "listener") process.on(signal, () => {});
process.kill(process.pid, signal);
// The process ends before it reaches the next turn of the event loop.
setTimeout(() => console.log("still running"), 2000);
