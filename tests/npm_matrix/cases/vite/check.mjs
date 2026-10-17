import fs from 'node:fs'; import { createServer } from 'vite';
const f = fs.readdirSync('dist/assets').find(x => x.endsWith('.js')); if (!fs.readFileSync('dist/assets/' + f, 'utf8').includes('vite-built')) process.exit(1);
const s = await createServer({ server: { port: 0, host: '127.0.0.1' }, logLevel: 'silent' }); await s.listen();
const port = s.httpServer.address().port; const t = await (await fetch(`http://127.0.0.1:${port}/main.js`)).text(); await s.close();
if (!t.includes('vite-built')) process.exit(2); console.log('SMOKE_OK');
