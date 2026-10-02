const seen: string[] = [];
const server = Deno.serve({
  hostname: "127.0.0.1",
  port: 0,
  onListen() {},
}, (req) => {
  seen.push(req.url);
  return new Response("handled");
});

async function send(target: string): Promise<string> {
  const conn = await Deno.connect({
    hostname: "127.0.0.1",
    port: server.addr.port,
  });
  await conn.write(
    new TextEncoder().encode(
      `GET ${target} HTTP/1.1\r\nhost: app\r\nconnection: close\r\n\r\n`,
    ),
  );
  let text = "";
  const buf = new Uint8Array(4096);
  while (true) {
    const n = await conn.read(buf);
    if (n === null) break;
    text += new TextDecoder().decode(buf.subarray(0, n));
  }
  conn.close();
  return text.split("\r\n")[0];
}

for (
  const target of [
    "http+memory://app/x",
    "HTTP+MEMORY://app/x",
    "http://app/x",
  ]
) {
  console.log(target, "->", await send(target));
}
console.log("handler saw:", JSON.stringify(seen));
await server.shutdown();
