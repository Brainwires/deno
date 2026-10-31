// The program's own main module asks to lift its limits.
try {
  console.log("disarmed", Deno.disarmResourceLimits());
} catch (e) {
  console.log(e.name);
}
