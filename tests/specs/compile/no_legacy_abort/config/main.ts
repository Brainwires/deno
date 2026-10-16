// A compiled app keeps the unstable no-legacy-abort setting it was compiled
// with: request.signal does not abort once a response was sent successfully,
// and Deno.serve prints no legacy-behavior notice on the first request. With
// the setting off, both happen, as under `deno run`.
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
