import { double } from "./double.ts";
export { Size } from "./size.ts";

export function calc(x: number): string {
  const y = double(x);
  if (y > 10) {
    return "big";
  }
  return "small";
}

export function unused(): string {
  return "never";
}
