// Playwright: `test` comes from a fixtures file, everything hangs off it.
import { expect, test } from './fixtures';

test.skip(({ browserName }) => browserName === 'webkit', 'flaky on webkit');

test.describe('checkout', () => {
  test.describe.configure({ mode: 'serial' });

  test.beforeEach(async ({ page }) => {
    await page.goto('/checkout');
  });

  test('shows the total', async ({ page }) => {
    await expect(page.getByTestId('total')).toHaveText('€30');
    await test.step('applies a coupon', async () => {
      await page.fill('#coupon', 'SAVE10');
      await expect(page.getByTestId('total')).toHaveText('€27');
    });
    await expect(page).toHaveScreenshot();
  });

  test('pays', { tag: '@smoke' }, async ({ page, isMobile }) => {
    test.skip(isMobile, 'no card form on mobile');
    await expect(page.getByRole('button', { name: 'Pay' })).toBeEnabled();
    expect(await page.title()).toContain('Checkout');
  });

  test.fixme('handles 3-D Secure', async ({ page }) => {
    await expect(page.locator('iframe')).toBeVisible();
  });
});

test.describe.serial('receipts', () => {
  test('emails a receipt', async ({ request }) => {
    const res = await request.get('/receipts');
    await expect(res).toBeOK();
  });
});
