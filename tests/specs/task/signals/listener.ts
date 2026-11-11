import { signals } from "./signals.ts";

for (const signal of signals) {
  Deno.addSignalListener(signal, () => {
    console.log("Received", signal);
    if (signal === "SIGTERM") {
      Deno.exit(0);
    }
  });
}

setInterval(() => {
  // keep alive
}, 1000);

// The pid lets the sender kill this process too if the test times out: it
// is `deno task`'s child, which a kill of `deno task` alone would orphan.
console.log("Ready", Deno.pid);
