import { expect, test } from 'vitest';
test('cart total', () => expect([2, 3].reduce((a, b) => a + b, 0)).toBe(5));
