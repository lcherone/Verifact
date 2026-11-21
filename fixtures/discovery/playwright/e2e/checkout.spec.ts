import { expect, test } from '@playwright/test';
test('checkout heading', async ({ page }) => {
  await page.setContent('<h1>Checkout</h1>');
  await expect(page.getByRole('heading')).toHaveText('Checkout');
});
