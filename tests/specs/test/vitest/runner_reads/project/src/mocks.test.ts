import { expect, it, vi } from "vitest";
import { greet } from "./greet.ts";
import { add, PI } from "./math.ts";

vi.mock("./greet.ts");
vi.mock("./math.ts");

it("uses the __mocks__ file next to the module", () => {
  expect(greet("you")).toBe("mocked you");
});

it("automocks a module", () => {
  expect(vi.isMockFunction(add)).toBe(true);
  expect(add(1, 2)).toBeUndefined();
  expect(PI).toBe(3.14);
});

it("automocks with vi.importMock", async () => {
  const mocked = await vi.importMock<typeof import("./not_imported.ts")>(
    "./not_imported.ts",
  );
  expect(mocked.value).toBe("not imported statically");
});
