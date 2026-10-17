import fs from 'node:fs'; if (!fs.readFileSync('dist/out.js','utf8').includes('webpacked')) process.exit(1); console.log('SMOKE_OK');
