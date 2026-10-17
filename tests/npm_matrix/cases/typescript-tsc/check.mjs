import fs from 'node:fs'; if (!fs.existsSync('dist/a.js')) process.exit(1); console.log('SMOKE_OK');
