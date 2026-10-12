import { expect, it, vi } from "vitest";
import { add, Calc, PI } from "./math.ts";
import { addTwice } from "./math_user.ts";
import { greet } from "./greet.ts";
import { double } from "./spied.ts";

vi.mock("./math.ts");
vi.mock("./greet.ts");
vi.mock("./spied.ts", { spy: true });

it("automocks a module without a factory", () => {
  expect(vi.isMockFunction(add)).toBe(true);
  expect(add(1, 2)).toBeUndefined();
  expect(addTwice(2)).toBeUndefined();
  expect(add).toHaveBeenCalledWith(2, 2);
  expect(PI).toBe(3.14);
  expect(new Calc().mul(2, 3)).toBeUndefined();
});

it("uses the __mocks__ file next to the module", () => {
  expect(greet("you")).toBe("mocked you");
});

it("spies on the module with { spy: true }", () => {
  expect(double(2)).toBe(4);
  expect(double).toHaveBeenCalledWith(2);
});
