const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const conn = await Deno.connect({
  hostname: "127.0.0.1",
  port: listener.addr.port,
});
conn.close();
listener.close();
console.log("done");
