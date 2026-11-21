import { expect, test } from 'vitest';
test('addition passes', () => expect(2 + 2).toBe(4));
test.skip('deliberately skipped', () => {});
