// Regression test: `JSR_URL` is read once, before any user code runs. It was
// read on the first `jsr:` specifier, so a value set with `Deno.env.set()`
// before a dynamic `import("jsr:...")` changed where the package was loaded
// from (the request still had to pass `--allow-import`).

// Two registries that only count requests: the one set at startup, and the
// one the program points `JSR_URL` at.
const hits = { startup: 0, runtime: 0 };
function registry(name: keyof typeof hits) {
  return Deno.serve(
    { hostname: "127.0.0.1", port: 0, onListen: () => {} },
    () => {
      hits[name]++;
      return new Response("not found", { status: 404 });
    },
  );
}
const startup = registry("startup");
const runtime = registry("runtime");
const startupUrl = `http://127.0.0.1:${(startup.addr as Deno.NetAddr).port}`;
const runtimeUrl = `http://127.0.0.1:${(runtime.addr as Deno.NetAddr).port}`;

const script = `
Deno.env.set("JSR_URL", ${JSON.stringify(runtimeUrl)});
const specifier = "jsr:@denotest/not-a-package";
try {
  await import(specifier);
} catch {
  // the registries know no packages
}
`;
const child = new Deno.Command(Deno.execPath(), {
  args: ["run", "--allow-env", "--allow-import", "-"],
  env: { JSR_URL: startupUrl, DENO_DIR: await Deno.makeTempDir() },
  stdin: "piped",
  stdout: "null",
  stderr: "null",
}).spawn();
const writer = child.stdin.getWriter();
await writer.write(new TextEncoder().encode(script));
await writer.close();
await child.status;

console.log(
  hits.runtime === 0
    ? "PASS: JSR_URL set at runtime was not used"
    : "FAIL: JSR_URL set at runtime was used",
);
console.log(
  hits.startup > 0
    ? "PASS: JSR_URL set at startup is used"
    : "FAIL: JSR_URL set at startup was not used",
);

await startup.shutdown();
await runtime.shutdown();
