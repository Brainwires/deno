import { expect, test } from "vitest";
import { viaMock } from "./with_mock.ts";

test("a module that mocks its own import", () => {
  expect(viaMock(2)).toBe(1000);
});
