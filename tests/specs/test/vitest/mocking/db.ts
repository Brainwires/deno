export function query(sql: string): string {
  return `real:${sql}`;
}
export const name = "real-db";
export default { kind: "real" };
