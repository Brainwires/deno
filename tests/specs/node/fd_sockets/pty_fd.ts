// The child inherits its terminal (a pty) as fd 3 from a shell, the way a
// terminal library hands a pty to a program, and reads it with
// `tty.ReadStream(3)`. An inherited terminal needs read and write access to
// everything, as any other descriptor `TTY(fd)` wraps; the flags given to
// this script are the child's.
const child = new URL("./pty_fd_child.mjs", import.meta.url).pathname;
const pty = Deno.openPty({
  cmd: "sh",
  args: [
    "-c",
    'exec "$0" run "$@" 3<>/dev/tty',
    Deno.execPath(),
    ...Deno.args,
    child,
  ],
  cols: 80,
  rows: 24,
  env: { NO_COLOR: "1" },
});
const writer = pty.writable.getWriter();
const decoder = new TextDecoder();
let out = "";
let sent = false;
const timer = setTimeout(() => {
  console.log("timed out:", JSON.stringify(out));
  Deno.exit(1);
}, 30_000);
for await (const chunk of pty.readable) {
  out += decoder.decode(chunk);
  if (!sent && out.includes("READY")) {
    sent = true;
    await writer.write(new TextEncoder().encode("ping\n"));
  }
  if (out.includes("child read ping") || out.includes("rror")) break;
}
clearTimeout(timer);
for (const line of out.replaceAll("\r", "").split("\n")) {
  const at = line.indexOf("child ");
  if (at !== -1) console.log(line.slice(at));
  else if (line.includes("rror")) console.log(line);
}
Deno.exit(0);
