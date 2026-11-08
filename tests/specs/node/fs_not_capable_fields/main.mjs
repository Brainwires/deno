import fs from "node:fs";

const file = "denied_dir/secret.txt";
const fields = (e) => ({
  code: e.code,
  permission: e.permission,
  resource: e.resource,
  syscall: e.syscall,
  path: e.path,
  notCapable: e instanceof Deno.errors.NotCapable,
});
const show = (name, e) => console.log(name, JSON.stringify(fields(e)));
const sync = (name, fn) => {
  try {
    fn();
    console.log(name, "no error");
  } catch (e) {
    show(name, e);
  }
};
const promise = async (name, fn) => {
  try {
    await fn();
    console.log(name, "no error");
  } catch (e) {
    show(name, e);
  }
};
const callback = (name, fn) =>
  new Promise((resolve) =>
    fn((e) => {
      if (e) show(name, e);
      else console.log(name, "no error");
      resolve();
    })
  );

// The allowed directory still reads.
console.log("allowed", fs.readFileSync("allowed/a.txt", "utf8").trim());

sync("readFileSync", () => fs.readFileSync(file));
await callback("readFile", (cb) => fs.readFile(file, cb));
await promise("promises.readFile", () => fs.promises.readFile(file));
sync("writeFileSync", () => fs.writeFileSync("allowed/new.txt", "x"));
await callback("writeFile", (cb) => fs.writeFile("allowed/new.txt", "x", cb));
await promise(
  "promises.writeFile",
  () => fs.promises.writeFile("allowed/new.txt", "x"),
);
sync("statSync", () => fs.statSync(file));
await callback("stat", (cb) => fs.stat(file, cb));
await promise("promises.stat", () => fs.promises.stat(file));
sync("lstatSync", () => fs.lstatSync(file));
sync("openSync", () => fs.openSync(file));
await callback("open", (cb) => fs.open(file, cb));
await promise("promises.open", () => fs.promises.open(file));
sync("readdirSync", () => fs.readdirSync("denied_dir"));
await callback("readdir", (cb) => fs.readdir("denied_dir", cb));
await promise("promises.readdir", () => fs.promises.readdir("denied_dir"));

// Deno's own APIs keep their error as it is.
try {
  Deno.readTextFileSync(file);
} catch (e) {
  console.log(
    "Deno.readTextFileSync",
    e.constructor.name,
    e instanceof Deno.errors.NotCapable,
    JSON.stringify(Object.keys(e)),
    "code" in e,
    e.message,
  );
}
