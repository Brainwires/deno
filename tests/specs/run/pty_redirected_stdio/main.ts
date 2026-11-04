const pty = Deno.openPty({
  cmd: "cmd.exe",
  args: ["/d", "/c", "echo pty-marker-42"],
  cols: 80,
  rows: 24,
});
const decoder = new TextDecoder();
let text = "";
for await (const chunk of pty.readable) {
  text += decoder.decode(chunk, { stream: true });
}
await pty.status;
console.log("the terminal saw it:", text.includes("pty-marker-42"));
