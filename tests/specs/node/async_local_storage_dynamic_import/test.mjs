// A module first loaded inside one test must not leave that test's async
// context active for the next test the runner starts.
import { als } from "./als.mjs";

function assertNoStore() {
  const store = als.getStore();
  if (store !== undefined) throw new Error(`leaked store: ${store}`);
}

Deno.test("plain module", async () => {
  const mod = await als.run("A", () => import("./fresh.mjs"));
  if (mod.atTop !== "A") throw new Error(`top level saw ${mod.atTop}`);
});
Deno.test("after plain module", assertNoStore);

Deno.test("top-level await module", async () => {
  const mod = await als.run("B", () => import("./fresh_tla.mjs"));
  if (mod.atTop !== "B" || mod.afterAwait !== "B") {
    throw new Error(`saw ${mod.atTop} / ${mod.afterAwait}`);
  }
});
Deno.test("after top-level await module", assertNoStore);

Deno.test("json module", async () => {
  await als.run(
    "C",
    () => import("./fresh_data.json", { with: { type: "json" } }),
  );
});
Deno.test("after json module", assertNoStore);
