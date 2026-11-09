import { attempt } from "./attempt.ts";

// A worker scoped to a link out of the parent's grants gets no more than the
// parent has there. (Its source is inline: the parent may not read the
// directory this module is in.)
const source = `
function attempt(name, fn) {
  try {
    fn();
    console.log(name + ": allowed");
  } catch (e) {
    console.log(name + ": " + (e instanceof Deno.errors.NotCapable ? "denied" : "error (" + e.name + ")"));
  }
}
attempt("worker readTextFile under dirlink", () => Deno.readTextFileSync("safe/dirlink/secret.txt"));
attempt("worker readTextFile outside", () => Deno.readTextFileSync("outside/secret.txt"));
attempt("worker readTextFile file", () => Deno.readTextFileSync("safe/file.txt"));
self.postMessage(null);
`;
const worker = new Worker(
  `data:application/javascript,${encodeURIComponent(source)}`,
  {
    type: "module",
    deno: { permissions: { read: ["safe/dirlink", "safe/file.txt"] } },
  },
);
const { promise, resolve } = Promise.withResolvers<void>();
worker.onmessage = () => resolve();
await promise;
worker.terminate();
attempt(
  "parent readTextFile under dirlink",
  () => Deno.readTextFileSync("safe/dirlink/secret.txt"),
);
