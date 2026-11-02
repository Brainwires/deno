interface Point {
  x: number;
  y: number;
}

type Label = "yes" | "no";

export function add(a: number, b: number): number {
  return a + b;
}

export function norm(p: Point): number {
  const square = (n: number): number => n * n;
  return Math.sqrt(square(p.x) + square(p.y));
}

export function unused(flag: boolean): Label {
  if (flag) {
    return "yes";
  }
  return "no";
}
