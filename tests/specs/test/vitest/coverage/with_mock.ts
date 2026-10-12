import { vi } from "vitest";
import { double } from "./double.ts";

vi.mock("./double.ts", () => ({ double: () => 1000 }));

export function viaMock(x: number): number {
  return double(x);
}
