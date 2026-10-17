const assert = require('assert'); describe('m', () => { it('works', async () => { await new Promise(r => setTimeout(r, 5)); assert.equal(2, 2); }); });
