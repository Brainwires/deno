import process from "node:process";

function read(label, fn) {
  try {
    console.log(`${label}: ${fn()}`);
  } catch (err) {
    console.log(
      `${label}: ${err instanceof Deno.errors.NotCapable ? "NotCapable" : err}`,
    );
  }
}

// op_inspector_port: checked.
read("process.debugPort", () => process.debugPort);
// op_inspector_enabled: not checked.
read("process._debugEnd()", () => {
  process._debugEnd();
  return "ok";
});
