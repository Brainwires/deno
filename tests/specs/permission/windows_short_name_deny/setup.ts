// base/longname gets an 8.3 alias (LONGNA~1 or a hashed form) from NTFS when
// 8dot3name creation is on for the volume (the default for the system
// volume); when it isn't, `fsutil file setshortname` gives it one (needs an
// administrator, which the CI runner is). short.txt holds the alias, or
// nothing when none could be had, with the reason in reason.txt.
Deno.mkdirSync("base/longname/b", { recursive: true });
Deno.writeTextFileSync("base/longname/b/prog", "packaged");
Deno.mkdirSync("base/longname/other");
Deno.mkdirSync("base/other");
const abs = `${Deno.cwd()}\\base\\longname`;
const decoder = new TextDecoder();

function shortName(): string {
  const { stdout } = new Deno.Command("powershell", {
    args: [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      `(New-Object -ComObject Scripting.FileSystemObject).GetFolder('${abs}').ShortName`,
    ],
  }).outputSync();
  return decoder.decode(stdout).trim();
}

let short = shortName();
let reason = "";
if (short.toLowerCase() === "longname") {
  const { stdout, stderr } = new Deno.Command("fsutil", {
    args: ["file", "setshortname", abs, "LONGNA~1"],
  }).outputSync();
  short = shortName();
  if (short.toLowerCase() === "longname") {
    reason = `no 8.3 alias for ${abs}; fsutil file setshortname: ${
      decoder.decode(stdout).trim()
    } ${decoder.decode(stderr).trim()}`.trim();
    short = "";
  }
}
Deno.writeTextFileSync("short.txt", short);
Deno.writeTextFileSync("reason.txt", reason);
console.log("setup done");
