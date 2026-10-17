// CommonJS's wrapper would reject this declaration, so Node runs the file as
// an ES module.
const require = (specifier) => "own require: " + specifier;
console.log(require("node:fs"));
