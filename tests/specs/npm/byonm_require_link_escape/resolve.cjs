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

const J = (...parts) => ["node_modules", ...parts].join("/");

report(
  "require.resolve",
  () => require.resolve("./" + J("evil", "mod.cjs")) && "resolved",
);
