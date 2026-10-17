const exists = (f) => {
  try {
    Deno.statSync(f);
    return true;
  } catch {
    return false;
  }
};
console.log(
  "root:",
  exists("post.txt"),
  "dependency:",
  exists("node_modules/@denotest/lifecycle-scripts-simple/message.js"),
);
