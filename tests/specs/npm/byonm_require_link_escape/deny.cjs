// A node_modules file stays readable without --allow-read (its real path is
// in a node_modules directory), but never past a --deny-read that covers it:
// by its absolute path, or as a package through a link in node_modules.
function report(label, fn) {
  try {
    const value = fn();
    console.log(`${label}: ${value.secret}`);
  } catch (error) {
    console.log(`${label}: ${error.name}`);
  }
}

// Computed, so nothing is resolved ahead of the run.
const file = ["node_modules", "x", "s.json"].join("/");
report("absolute", () => require(`${__dirname}/other/${file}`));
report("link", () => require(`far/${file}`));
