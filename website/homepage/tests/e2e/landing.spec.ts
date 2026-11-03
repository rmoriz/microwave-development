import { expect, test } from '@playwright/test';

const widths = [320, 375, 768, 900, 1440];

// The page ships without a script; it has to stand on its own.
test.use({ javaScriptEnabled: false });

test('the landing page renders with the site design', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('h1')).toHaveCount(1);
  // The staged page must carry the substituted site URL, not the placeholder.
  await expect(page.locator('.logo')).toHaveAttribute(
    'href',
    'https://rmoriz.github.io/microwave-development/',
  );
  await expect(page.locator('.hero-actions a', { hasText: 'Website' })).toHaveAttribute(
    'href',
    'https://rmoriz.github.io/microwave-development/',
  );
  await expect(page.locator('body')).not.toContainText('{{website_url}}');
  const background = await page
    .locator('body')
    .evaluate((body) => getComputedStyle(body).backgroundColor);
  expect(background).toBe('rgb(42, 77, 110)');
  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow, `overflow at ${width}px`).toBeLessThanOrEqual(1);
  }
});
