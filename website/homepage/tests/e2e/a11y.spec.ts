import { test, expect } from '@playwright/test';
import { url } from './base';
import AxeBuilder from '@axe-core/playwright';

for (const width of [375, 1440]) {
  test(`no serious accessibility violations at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(url());
    const results = await new AxeBuilder({ page }).analyze();
    const serious = results.violations.filter(
      (violation) =>
        violation.impact === 'serious' || violation.impact === 'critical',
    );
    expect(serious).toEqual([]);
  });
}

test('selection stays visible on cream surfaces', async ({ page }) => {
  await page.goto(url());
  for (const selector of ['a.button-primary', 'a.skip-link']) {
    const style = await page
      .locator(selector)
      .first()
      .evaluate((el) => {
        const pseudo = getComputedStyle(el, '::selection');
        return [pseudo.backgroundColor, pseudo.color];
      });
    expect(style).toEqual(['rgb(61, 220, 255)', 'rgb(42, 77, 110)']);
  }
});

test('focus ring stays visible on the blue surfaces', async ({ page }) => {
  await page.goto(url());
  const cta = page.locator('a.button-primary').first();
  for (let i = 0; i < 30; i += 1) {
    if (await cta.evaluate((el) => el === document.activeElement)) break;
    await page.keyboard.press('Tab');
  }
  await expect(cta).toBeFocused();
  await expect(cta).toHaveCSS('outline-color', 'rgb(61, 220, 255)');
});

test('skip link reaches the main landmark', async ({ page }) => {
  await page.goto(url());
  await page.keyboard.press('Tab');
  await expect(page.locator('a.skip-link')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#main$/);
  expect(await page.evaluate(() => document.activeElement?.id)).toBe('main');
});
