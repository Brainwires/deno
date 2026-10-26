// prints what the package's install script recorded
console.log(
  Deno.readTextFileSync(
    "node_modules/@denotest/jail-native-addon/results.txt",
  ).trim(),
);
