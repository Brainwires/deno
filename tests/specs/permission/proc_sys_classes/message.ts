for (const path of ["/proc/self/status", "/proc"]) {
  try {
    Deno.readFileSync(path);
    console.log(`${path}: read`);
  } catch (e) {
    console.log((e as Error).message);
  }
}
