// Whether anything from denied/package.json reaches the program: the
// resolved path, or an error naming it.
function report(label, fn) {
  let text;
  try {
    text = String(fn());
  } catch (error) {
    text = String(error.message);
  }
  console.log(`${label}: ${text.includes("LEAKED_VALUE") ? "seen" : "hidden"}`);
}
report("require.resolve", () => require.resolve("pkg/sub"));
