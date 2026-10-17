const exists = (path: string) => {
  try {
    Deno.statSync(path);
    return true;
  } catch {
    return false;
  }
};
console.log(
  "arm64:",
  exists("node_modules/@denotest/multiple-arches-win32-arm64/package.json"),
);
console.log(
  "x64:",
  exists("node_modules/@denotest/multiple-arches-win32-x64/package.json"),
);
