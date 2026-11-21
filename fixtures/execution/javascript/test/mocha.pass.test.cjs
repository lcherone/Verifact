const assert = require('node:assert/strict');
describe('arithmetic', () => {
  it('addition passes', () => assert.equal(2 + 2, 4));
  it.skip('deliberately skipped', () => {});
});
