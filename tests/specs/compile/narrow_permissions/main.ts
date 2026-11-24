for (const file of ["data/a.txt", "other/b.txt"]) {
  try {
    Deno.readTextFileSync(file);
    console.log(file, "read");
  } catch (err) {
    console.log(file, (err as Error).name);
  }
}
try {
  console.log("NARROW_TEST", Deno.env.get("NARROW_TEST") ?? "unset");
} catch (err) {
  console.log("NARROW_TEST", (err as Error).name);
}
