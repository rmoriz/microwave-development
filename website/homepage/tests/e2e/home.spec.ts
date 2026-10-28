import { test, expect } from '@playwright/test';

const widths = [320, 375, 768, 1440];

test('no horizontal overflow at narrow and wide viewports', async ({ page }) => {
  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/');
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    expect(overflow).toBeLessThanOrEqual(1);
  }
});

test('mobile navigation opens and closes without JavaScript', async ({ browser }) => {
  const context = await browser.newContext({
    javaScriptEnabled: false,
    viewport: { width: 375, height: 900 },
  });
  const page = await context.newPage();
  await page.goto('/');
  const summary = page.locator('.nav-mobile summary');
  await expect(summary).toBeVisible();
  await summary.click();
  await expect(page.locator('.nav-mobile a[href="/#product"]')).toBeVisible();
  await expect(page.locator('.nav-mobile a[href="/#agents"]')).toBeVisible();
  await context.close();
});

test('anchor links clear the sticky header', async ({ page }) => {
  await page.goto('/');
  await page.locator('.nav-desktop a[href="/#product"]').click();
  await expect(page).toHaveURL(/#product$/);
  const top = await page.locator('#product').evaluate((element) => {
    return element.getBoundingClientRect().top;
  });
  expect(top).toBeGreaterThanOrEqual(72);

  await page.locator('.nav-desktop a[href="/#agents"]').click();
  await expect(page).toHaveURL(/#agents$/);
  const agentTop = await page.locator('#agents').evaluate((element) => {
    return element.getBoundingClientRect().top;
  });
  expect(agentTop).toBeGreaterThanOrEqual(72);
});

test('legal pages keep working primary navigation', async ({ page }) => {
  await page.goto('/privacy/');
  await expect(page.locator('header.site-header')).toBeVisible();
  await page.locator('.nav-desktop a[href="/#product"]').click();
  await expect(page).toHaveURL(/\/#product$/);
  await expect(page.locator('#product')).toBeVisible();
});

test('technical accordions are keyboard operable', async ({ page }) => {
  await page.goto('/');
  const first = page.locator('#under-the-hood details').first();
  const summary = first.locator('summary');
  await summary.focus();
  await page.keyboard.press('Enter');
  await expect(first).toHaveAttribute('open', '');
});

test('JavaScript-disabled content is complete', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto('/');
  await expect(page.locator('h1')).toHaveText('Get cooking without becoming a chef.');
  await expect(page.locator('#agents')).toBeVisible();
  await expect(page.locator('#support')).toBeVisible();
  await context.close();
});

test('reduced motion disables reveals and smooth scrolling', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/');
  expect(await page.locator('.reveal').count()).toBeGreaterThan(0);
  const styles = await page.evaluate(() => {
    const root = getComputedStyle(document.documentElement);
    const reveal = document.querySelector('.reveal');
    return {
      scrollBehavior: root.scrollBehavior,
      revealAnimation: reveal ? getComputedStyle(reveal).animationName : 'none',
    };
  });
  expect(styles.scrollBehavior).toBe('auto');
  expect(styles.revealAnimation).toBe('none');
});

test('page emits no console errors', async ({ page }) => {
  const errors: string[] = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  await page.goto('/');
  await page.waitForLoadState('networkidle');
  expect(errors).toEqual([]);
});
