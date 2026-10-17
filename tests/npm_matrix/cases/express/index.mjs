import express from "express";
const app = express(); app.use(express.json());
app.get("/x/:id", (req, res) => res.json({ id: req.params.id, q: req.query.a }));
app.post("/echo", (req, res) => res.json(req.body));
const s = app.listen(0, async () => {
  const p = s.address().port;
  const a = await (await fetch(`http://127.0.0.1:${p}/x/7?a=1`)).json();
  const b = await (await fetch(`http://127.0.0.1:${p}/echo`, { method: "POST", headers: { "content-type": "application/json" }, body: '{"k":2}' })).json();
  if (a.id !== "7" || a.q !== "1" || b.k !== 2) throw new Error(JSON.stringify([a, b]));
  s.close(); console.log("SMOKE_OK");
});
