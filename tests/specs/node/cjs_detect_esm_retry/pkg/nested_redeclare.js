// A declaration in a nested scope is fine in CommonJS.
function local() {
  const require = () => "local";
  return require();
}
console.log("cjs", local(), typeof module.exports);
