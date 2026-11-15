Deno.mkdirSync("bin", { recursive: true });
Deno.mkdirSync("src/lib", { recursive: true });

Deno.writeTextFileSync(
  "src/main.ts",
  `import { x } from "./lib/x.ts";

export default {
  onListen() {
    console.log(x);
    Deno.exit(0);
  },
  fetch() {
    return new Response(x);
  },
};
`,
);
Deno.writeTextFileSync("src/lib/x.ts", "export const x = 'ok';\n");
Deno.symlinkSync("../src/main.ts", "bin/main.ts");
