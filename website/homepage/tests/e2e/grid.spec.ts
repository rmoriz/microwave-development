import { test, expect } from '@playwright/test';

test('grid sits behind content without taking clicks', async ({ page }) => {
  await page.goto('/');
  const field = page.locator('.grid-field');
  await expect(field).toHaveCount(1);
  await expect(field).toHaveAttribute('aria-hidden', 'true');
  await expect(field).toHaveCSS('pointer-events', 'none');
  await expect(field).toHaveCSS('position', 'fixed');
  await page.locator('h1').click();
  const samples = await page.evaluate(() =>
    [
      [4, 4],
      [720, 300],
      [720, 700],
      [1200, 500],
    ].map(([x, y]) => document.elementFromPoint(x, y)?.className ?? ''),
  );
  for (const className of samples) {
    expect(className).not.toContain('grid-field');
  }
});

test('grid holds still under reduced motion and on phones', async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/');
  const plane = page.locator('.grid-field__plane');
  await expect(plane).toHaveCSS('animation-name', 'none');
  expect(await plane.evaluate((el) => getComputedStyle(el).transform)).toMatch(
    /^matrix3d/,
  );

  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await page.setViewportSize({ width: 375, height: 800 });
  await page.goto('/');
  await expect(plane).toHaveCSS('animation-name', 'none');
  await expect(plane).toHaveCSS('opacity', '0.1');
});

test('grid runs on desktop with motion allowed', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/');
  await expect(page.locator('.grid-field__plane')).toHaveCSS(
    'animation-name',
    'grid-flow',
  );
  await expect(page.locator('.grid-field__plane')).toHaveCSS('opacity', '0.15');
});
