Deno.addSignalListener("SIGTERM", () => {
  console.log("listener ran");
  Deno.exit(3);
});
Deno.serve({ port: 0, onListen: () => console.log("ready") }, () => {
  return new Response("hello");
});
