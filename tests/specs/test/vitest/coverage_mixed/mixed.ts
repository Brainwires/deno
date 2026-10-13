export function kind(x: number): string {
  if (x < 0) {
    return "negative";
  }
  return "positive";
}

export function plainOnly(): string {
  return "plain";
}

export function vitestOnly(): string {
  return "vitest";
}
