import { dep } from "./dep.ts";

export function usesDep(x: number): number {
  return dep(x) + 1;
}
