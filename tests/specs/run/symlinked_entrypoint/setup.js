Deno.mkdirSync("bin", { recursive: true });
Deno.mkdirSync("src/lib", { recursive: true });

Deno.writeTextFileSync(
  "src/tool.js",
  "import { x } from './lib/x.js';\nconsole.log(x);\n",
);
Deno.writeTextFileSync("src/lib/x.js", "export const x = 'ok';\n");
Deno.symlinkSync("../src/tool.js", "bin/tool");
