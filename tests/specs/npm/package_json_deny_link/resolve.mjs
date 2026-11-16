// The same through the ES module resolver.
let text;
try {
  text = import.meta.resolve("pkg/sub");
} catch (error) {
  text = String(error.message);
}
console.log(
  `import.meta.resolve: ${text.includes("LEAKED_VALUE") ? "seen" : "hidden"}`,
);
