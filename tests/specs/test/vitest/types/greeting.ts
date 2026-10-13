export const prefix: string = "Hello";

export function greet(name: string): string {
  return `${prefix}, ${name}`;
}

export const store = {
  load(id: number): Promise<{ id: number; name: string }> {
    return Promise.resolve({ id, name: `user ${id}` });
  },
  get size(): number {
    return 1;
  },
};
