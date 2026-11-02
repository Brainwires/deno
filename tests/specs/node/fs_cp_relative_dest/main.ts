import fs from "node:fs";

Deno.mkdirSync("src/sub", { recursive: true });
Deno.writeTextFileSync("src/f", "f");
Deno.writeTextFileSync("src/sub/g", "g");

function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return `error: ${(e as Error).message}`;
  }
}
async function outcomeAsync(fn: () => Promise<void>) {
  try {
    await fn();
    return "done";
  } catch (e) {
    return `error: ${(e as Error).message}`;
  }
}
function listing(dir: string) {
  try {
    return [...Deno.readDirSync(dir)].map((e) => e.name).sort().join(",");
  } catch {
    return "absent";
  }
}
function content(path: string) {
  try {
    return Deno.readTextFileSync(path);
  } catch {
    return "absent";
  }
}

console.log(
  "cpSync a folder:",
  outcome(() => fs.cpSync("src", "dst", { recursive: true })),
);
console.log("  dst:", listing("dst"));
console.log("cpSync a file:", outcome(() => fs.cpSync("src/f", "g")));
console.log("  g:", content("g"));
console.log(
  "cp a file:",
  await outcomeAsync(() => fs.promises.cp("src/f", "h")),
);
console.log("  h:", content("h"));
console.log(
  "cp a folder:",
  await outcomeAsync(() => fs.promises.cp("src", "dst2", { recursive: true })),
);
console.log("  dst2:", listing("dst2"));
console.log(
  "cp (callback) a file:",
  await outcomeAsync(() =>
    new Promise<void>((resolve, reject) =>
      fs.cp("src/f", "i", (err) => err ? reject(err) : resolve())
    )
  ),
);
console.log("  i:", content("i"));
