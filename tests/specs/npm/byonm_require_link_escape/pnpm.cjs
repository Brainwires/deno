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

report("pnpm", () => require(["p", "k", "g"].join("")));
