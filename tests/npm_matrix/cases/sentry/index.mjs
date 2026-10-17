import http from "node:http";
let hits = 0; const s = http.createServer((q, r) => { hits++; q.resume(); r.end("{}"); }); await new Promise(r => s.listen(0, r));
const Sentry = await import("@sentry/node");
Sentry.init({ dsn: `http://pub@127.0.0.1:${s.address().port}/1`, tracesSampleRate: 1 });
Sentry.captureException(new Error("boom")); await Sentry.flush(3000); s.close();
if (!hits) { console.error("no envelope sent"); process.exit(1); } console.log("SMOKE_OK");
