// Runs a few children with stdout piped and checks that each one's output
// arrived whole and its exit code is the one it asked for.
async function run(args: string[]) {
  const out = await new Deno.Command(Deno.execPath(), {
    args,
    stdout: "piped",
    stderr: "inherit",
  }).output();
  return { code: out.code, stdout: out.stdout };
}

const evals: [string, string][] = [
  ["console.log", `console.log("x".repeat(1 << 20)); Deno.exit(42);`],
  [
    "writeSync",
    `Deno.stdout.writeSync(new TextEncoder().encode("y".repeat(65536)));`,
  ],
  ["end of program", `console.log("z".repeat(4096)); Deno.exitCode = 7;`],
];
for (const [name, code] of evals) {
  const { code: exitCode, stdout } = await run(["eval", code]);
  console.log(`${name}: exit code ${exitCode}, ${stdout.length} bytes`);
}

for (const [name, args] of [["version", ["-V"]], ["info", ["info"]]]) {
  const { code: exitCode, stdout } = await run(args as string[]);
  const text = new TextDecoder().decode(stdout);
  console.log(
    `${name}: exit code ${exitCode}, ${
      text.endsWith("\n") ? "ends with a newline" : JSON.stringify(text)
    }`,
  );
}
