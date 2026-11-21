import test from 'node:test';
import assert from 'node:assert/strict';
test('addition passes', () => assert.equal(2 + 2, 4));
test.skip('deliberately skipped', () => {});
