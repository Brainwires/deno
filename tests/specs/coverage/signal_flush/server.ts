function handle(_req: Request): Response {
  return new Response("hello");
}
Deno.serve({ port: 0, onListen: () => console.log("ready") }, handle);
