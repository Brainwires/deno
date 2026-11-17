import fs from "node:fs";

function outcome(err: unknown): string {
  if (!err) return "allowed (BUG)";
  return (err as Error).name === "NotCapable"
    ? "denied"
    : `error (${(err as Error).name})`;
}

Deno.mkdirSync("denied");
const fd = fs.openSync("denied/w.txt", "w");
try {
  fs.fstatSync(fd);
  console.log("fstatSync:", outcome(null));
} catch (err) {
  console.log("fstatSync:", outcome(err));
}
await new Promise<void>((resolve) => {
  fs.fstat(fd, (err) => {
    console.log("fstat:", outcome(err));
    resolve();
  });
});
// the descriptor still does what it was opened for
console.log("write:", fs.writeSync(fd, "x"));
fs.closeSync(fd);

// a descriptor without a path is not checked
console.log("stdout:", typeof fs.fstatSync(1).mode);
