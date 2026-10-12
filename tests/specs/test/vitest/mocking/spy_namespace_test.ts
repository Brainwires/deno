import { afterEach, expect, it, vi } from "vitest";
import * as defaults from "./defaults.ts";
import * as reexport from "./reexport.ts";
import { open } from "./open.ts";
import { double } from "./spied.ts";

afterEach(() => {
  vi.restoreAllMocks();
});

it("spies on an export of a module namespace", () => {
  const spy = vi.spyOn(defaults, "browserName").mockReturnValue("spied");
  vi.spyOn(defaults, "profile").mockReturnValue("spied-profile");
  expect(open()).toBe("spied/spied-profile");
  expect(spy).toHaveBeenCalledOnce();
});

it("restores the export", () => {
  expect(open()).toBe("real/real-profile");
});

it("spies through re-exports", () => {
  vi.spyOn(reexport, "browserName").mockReturnValue("re");
  vi.spyOn(reexport, "double").mockReturnValue(0);
  expect(open()).toBe("re/real-profile");
  expect(double(3)).toBe(0);
});
