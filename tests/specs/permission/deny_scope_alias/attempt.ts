export function attempt(name: string, fn: () => unknown) {
  try {
    fn();
    console.log(`${name}: allowed`);
  } catch (e) {
    if (e instanceof Deno.errors.NotCapable) {
      console.log(`${name}: denied`);
    } else {
      console.log(`${name}: error (${(e as Error).name})`);
    }
  }
}
