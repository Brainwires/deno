import { expect, it, jest } from "@jest/globals";
import { run } from "./service.ts";

jest.mock("./db.ts", () => ({
  query: () => "jest",
  name: "jest",
  default: { kind: "jest" },
}));

it("jest.mock replaces the module", () => {
  expect(run()).toBe("jest|jest|jest");
});
