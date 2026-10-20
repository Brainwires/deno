// Waits the given number of milliseconds without using CPU, then says so.
console.log("start");
setTimeout(() => console.log("done"), Number(Deno.args[0]));
