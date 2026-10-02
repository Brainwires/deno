const server = Deno.serve(() => new Response("ok"));
await server.shutdown();
