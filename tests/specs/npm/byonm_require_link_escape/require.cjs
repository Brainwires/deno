// What a require-side call did: its value, or the error's name.
function report(label, fn) {
  try {
    const value = fn();
    console.log(
      `${label}: ${typeof value === "object" ? value.secret : value}`,
    );
  } catch (error) {
    console.log(`${label}: ${error.name}`);
  }
}

// Computed, so nothing is resolved ahead of the run.
const J = (...parts) => ["node_modules", ...parts].join("/");

report("relative", () => require("./" + J("evil", "config.json")));
report("absolute", () => require(__dirname + "/" + J("evil", "mod.cjs")));
