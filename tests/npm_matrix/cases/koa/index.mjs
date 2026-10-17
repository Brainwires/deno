import Koa from "koa"; import Router from "@koa/router";
const app = new Koa(); const r = new Router();
r.get("/a/:x", (ctx) => { ctx.body = { x: ctx.params.x }; });
app.use(r.routes());
const s = app.listen(0, async () => {
  const j = await (await fetch(`http://127.0.0.1:${s.address().port}/a/5`)).json();
  if (j.x !== "5") throw new Error("koa"); s.close(); console.log("SMOKE_OK");
});
