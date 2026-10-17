import { Hono } from "hono"; import { serve } from "@hono/node-server";
const app = new Hono(); app.get("/", (c) => c.json({ ok: 1 }));
const s = serve({ fetch: app.fetch, port: 0 }, async (info) => {
  const j = await (await fetch(`http://127.0.0.1:${info.port}/`)).json();
  if (j.ok !== 1) throw new Error("hono"); s.close(); console.log("SMOKE_OK");
});
