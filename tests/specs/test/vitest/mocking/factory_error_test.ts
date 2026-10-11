import { it, vi } from "vitest";
import { run } from "./service.ts";

vi.mock("./db.ts", () => 1 as never);

it("never collected", () => {
  run();
});
