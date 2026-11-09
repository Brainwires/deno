// Every heap profile here is a sampling heap profile with Node's file name.
const expected = Number(Deno.args[0]);
const files = [...Deno.readDirSync(".")]
  .map((f) => f.name)
  .filter((name) => name.endsWith(".heapprofile"))
  .sort();
const pattern = /^Heap\.\d{8}\.\d{6}\.(\d+)\.(\d+)\.(\d{3})\.heapprofile$/;
for (const name of files) {
  const match = pattern.exec(name);
  if (!match) throw new Error(`unexpected name ${name}`);
  const profile = JSON.parse(Deno.readTextFileSync(name));
  if (!profile.head || !Array.isArray(profile.samples)) {
    throw new Error(`${name} is not a heap profile`);
  }
}
if (files.length !== expected) {
  throw new Error(`expected ${expected} profiles, got ${files}`);
}
// The main thread's profile: thread id 0, the process's first file.
const main = files.find((name) => pattern.exec(name)[2] === "0");
if (!main || pattern.exec(main)[3] !== "001") {
  throw new Error(`no main-thread profile in ${files}`);
}
console.log(
  `${files.length} heap profile(s) Heap.<date>.<time>.<pid>.0.001.heapprofile`,
);
