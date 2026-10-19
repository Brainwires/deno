// prints what the probe package's lifecycle script recorded, and which
// files outside node_modules it managed to write
const name = Deno.args[0];
console.log(
  Deno.readTextFileSync(`node_modules/@denotest/${name}/results.txt`).trim(),
);
const written = [
  "pwned-node.txt",
  "pwned-sh.txt",
  "pwned-shlink.txt",
  "pwned-deno.txt",
  "member/pwned-node-link.txt",
  "member/pwned-sh-link.txt",
].filter((file) => {
  try {
    Deno.statSync(file);
    return true;
  } catch {
    return false;
  }
});
console.log(
  `written outside: ${written.length ? written.join(", ") : "nothing"}`,
);
