export function attempt(name: string, fn: () => unknown) {
  try {
    const value = fn();
    console.log(`${name}: ${typeof value === "string" ? value : "allowed"}`);
  } catch (e) {
    if (e instanceof Deno.errors.NotCapable) {
      console.log(`${name}: denied`);
    } else {
      console.log(`${name}: error (${(e as Error).name})`);
    }
  }
}
