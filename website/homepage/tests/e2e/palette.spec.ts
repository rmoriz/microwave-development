import { test, expect } from '@playwright/test';

test('body and panels use the steel-blue ramp', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('body')).toHaveCSS(
    'background-color',
    'rgb(42, 77, 110)',
  );
  await expect(page.locator('.gaps-section')).toHaveCSS(
    'background-color',
    'rgb(47, 83, 117)',
  );
});
