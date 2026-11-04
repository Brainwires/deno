// `npm:node-pty` from the registry: installed into node_modules without its
// install script (which would build the addon), then served by the runtime's
// implementation over Deno.openPty.
import pty from "npm:node-pty@1.1.0";

const term = pty.spawn("sh", ["-c", "echo via registry"], {
  cols: 80,
  rows: 24,
});
let out = "";
term.onData((data: string) => out += data);
term.onExit(({ exitCode }: { exitCode: number }) => {
  console.log(JSON.stringify(out), exitCode);
});
