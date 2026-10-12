// project/src/linked -> outside (a directory outside the project)
Deno.symlinkSync(
  new URL("./outside", import.meta.url),
  new URL("./project/src/linked", import.meta.url),
);
