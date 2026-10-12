export function double(x: number): number {
  if (x < 0) {
    throw new Error("negative");
  }
  return x * 2;
}
