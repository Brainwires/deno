// A link made inside the extraction directory that leads into a
// --deny-read scope: the CommonJS loader's file reads through it are
// checked like any other host read, as Deno.readTextFileSync is.
// `require()` resolves the link first; `Module._extensions` reads the path
// it is given.
import Module, { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const link = `${import.meta.dirname}/escape_link`;
try {
  Deno.removeSync(link);
} catch {
  // not there yet
}
Deno.symlinkSync(`${Deno.cwd()}/secret`, link);
const extensions = (Module as unknown as {
  _extensions: Record<string, (m: { exports: unknown }, f: string) => void>;
})._extensions;
for (
  const [label, read] of [
    ["require", () => require(`${link}/s.json`).secret],
    ["_extensions json", () => {
      const m = { exports: {} as { secret?: string } };
      extensions[".json"](m, `${link}/s.json`);
      return m.exports.secret;
    }],
    ["readTextFileSync", () => Deno.readTextFileSync(`${link}/s.json`)],
  ] as const
) {
  try {
    console.log(`${label}: ${read()}`);
  } catch (error) {
    console.log(`${label}: ${(error as Error).name}`);
  }
}
Deno.removeSync(link);
