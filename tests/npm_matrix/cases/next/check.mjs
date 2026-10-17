const port = process.argv[2];
for (let i = 0; i < 60; i++) { try { const t = await (await fetch(`http://127.0.0.1:${port}/`)).text(); const j = await (await fetch(`http://127.0.0.1:${port}/api/x`)).json(); if (t.includes("hello-next") && j.ok) { console.log("SMOKE_OK"); process.exit(0); } } catch {} await new Promise(r => setTimeout(r, 1000)); }
process.exit(1);
