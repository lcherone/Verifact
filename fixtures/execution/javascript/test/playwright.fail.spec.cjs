const { test, expect } = require('@playwright/test');
test('assertion fails visibly', () => expect(2 + 2).toBe(5));
