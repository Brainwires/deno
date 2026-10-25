import { setTraceSigInt } from "node:util";

if (Deno.env.get("CHILD_TRACE_API") === "1") {
  setTraceSigInt(true);
}

function innerWork() {
  let sum = 0;
  // Busy, so the interrupt lands in this frame.
  for (let i = 0; i < Number.MAX_SAFE_INTEGER; i++) {
    sum += i;
    if (i === 1000) console.log("ready");
  }
  return sum;
}

function doWork() {
  return innerWork();
}

switch (Deno.env.get("CHILD_MODE")) {
  case "busy":
    doWork();
    break;
  case "idle":
    setInterval(() => {}, 1000);
    console.log("ready");
    break;
  case "listener":
    Deno.addSignalListener("SIGINT", () => {
      console.log("listener ran");
      Deno.exit(7);
    });
    setInterval(() => {}, 1000);
    console.log("ready");
    break;
}
