const { test, expect } = require('@playwright/test');
test('addition passes without needing a browser fixture', () => expect(2 + 2).toBe(4));
test.skip('deliberately skipped', () => {});
