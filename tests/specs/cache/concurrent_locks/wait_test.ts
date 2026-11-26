import { covered } from "./covered.ts";

Deno.test("waits for the release file", async () => {
  covered();
  while (true) {
    try {
      Deno.statSync("release");
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  }
});
