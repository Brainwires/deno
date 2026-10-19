// Writes the modules that import by absolute `file:` URL (the URLs depend on
// where the test runs).
const a = import.meta.resolve("./lib/a.ts");
const b = import.meta.resolve("../outside/b.ts");
Deno.writeTextFileSync(
  "ok.ts",
  `import { x } from "${a}";\nimport { y } from "${b}";\n` +
    `export const n: number = x + y;\n`,
);
Deno.writeTextFileSync(
  "bad.ts",
  `import { y } from "${b}";\nexport const s: string = y;\n`,
);
Deno.writeTextFileSync(
  "deno.json",
  JSON.stringify({
    imports: {
      "lib/": import.meta.resolve("./lib/"),
      "outside-b": b,
    },
  }),
);
Deno.writeTextFileSync(
  "alias.ts",
  `import { x } from "lib/a.ts";\nimport { y } from "outside-b";\n` +
    `export const n: number = x + y;\n`,
);
console.log("written");
