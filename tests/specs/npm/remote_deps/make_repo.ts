// Creates a git repository (tags v1.0.0, v1.1.0 and v2.0.0) and the apps'
// package.json, whose git dependencies need its absolute path.
const repo = `${Deno.cwd()}/repo`;
// a file URL on every OS (`file:///C:/...` on Windows)
const repoUrl = new URL("./repo", import.meta.url).href;
function git(...args: string[]) {
  const { code, stderr } = new Deno.Command("git", {
    args: [
      "-c",
      "user.name=test",
      "-c",
      "user.email=test@example.com",
      ...args,
    ],
    cwd: repo,
  }).outputSync();
  if (code !== 0) throw new Error(new TextDecoder().decode(stderr));
}
Deno.mkdirSync(repo);
git("init", "-q", "-b", "main");
for (const version of ["1.0.0", "1.1.0", "2.0.0"]) {
  Deno.writeTextFileSync(
    `${repo}/package.json`,
    JSON.stringify({
      name: "git-dep",
      version,
      main: "index.js",
      bin: { "git-dep-cli": "./cli.js" },
      dependencies: { "@denotest/add": "1.0.0" },
    }),
  );
  Deno.writeTextFileSync(
    `${repo}/index.js`,
    `module.exports = { version: "${version}", sum: require("@denotest/add").add(1, 1) };\n`,
  );
  Deno.writeTextFileSync(`${repo}/cli.js`, "#!/usr/bin/env node\n");
  git("add", ".");
  git("commit", "-q", "-m", version);
  git("tag", `v${version}`);
}
const packageJson = JSON.stringify({
  name: "remote-deps-app",
  version: "1.0.0",
  dependencies: {
    "git-dep": `git+${repoUrl}#v1.0.0`,
    "git-semver": `git+${repoUrl}#semver:^1.0.0`,
    "local-tool": "file:./local-tool",
    "tarball-dep": "http://localhost:4260/@denotest/tarball-dep/1.0.0.tgz",
  },
});
for (const app of ["app", "app_isolated"]) {
  Deno.writeTextFileSync(`${app}/package.json`, packageJson);
}
