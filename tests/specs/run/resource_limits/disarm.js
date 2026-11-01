// The program's own main module asks to lift its limits.
try {
  console.log("disarmed", Deno.disarmResourceLimits(import.meta));
} catch (e) {
  console.log(e.name);
}
