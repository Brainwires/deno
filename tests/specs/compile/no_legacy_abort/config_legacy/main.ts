// A compiled app keeps the request.signal behavior it was compiled with
// (DENEXT.md item 102): by default request.signal does not abort once a
// response was sent successfully; compiled with the legacy-abort opt-out (or
// run with DENO_LEGACY_ABORT=1) it does, as under `deno run`. Neither prints
// a notice.
let signal: AbortSignal | undefined;
const aborted = Promise.withResolvers<void>();

const server = Deno.serve({ port: 0, onListen: () => {} }, (req) => {
  signal = req.signal;
  signal.addEventListener("abort", () => aborted.resolve());
  return new Response("ok");
});

const res = await fetch(`http://127.0.0.1:${server.addr.port}/`);
const body = await res.text();

let timer: ReturnType<typeof setTimeout>;
const result = await Promise.race([
  aborted.promise.then(() => true),
  new Promise((r) => timer = setTimeout(r, 1000)).then(() => false),
]);
clearTimeout(timer!);
console.log("body:", body);
console.log("signal aborted after a successful response:", result);

await server.shutdown();
