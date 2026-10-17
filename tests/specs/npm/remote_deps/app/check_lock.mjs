const lock = JSON.parse(Deno.readTextFileSync("deno.lock"));
const repo = new URL("../repo", import.meta.url).pathname;
for (const [specifier, value] of Object.entries(lock.npmRemote ?? {})) {
  const kind = /^[0-9a-f]{40}$/.test(value)
    ? "commit"
    : /^sha512-/.test(value)
    ? "integrity"
    : value;
  console.log(specifier.replace(repo, "<repo>"), kind);
}
