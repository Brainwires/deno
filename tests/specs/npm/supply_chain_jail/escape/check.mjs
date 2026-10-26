// prints what the escape probe recorded, then whether the process it left
// behind outlived the install, and whether `open` ran its script
const dir = "node_modules/@denotest/jail-escape-probe";
console.log(Deno.readTextFileSync(`${dir}/results.txt`).trim());
const pid = Deno.readTextFileSync(`${dir}/sleeper.pid`).trim();
const { success } = new Deno.Command("ps", { args: ["-p", pid] }).outputSync();
console.log(`sleeper after install: ${success ? "alive" : "gone"}`);
let escaped = false;
try {
  Deno.statSync("escaped-open");
  escaped = true;
} catch {
  // not written
}
console.log(`open ran its script: ${escaped ? "yes" : "no"}`);
