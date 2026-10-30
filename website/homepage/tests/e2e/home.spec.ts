import { test, expect } from '@playwright/test';
import { url } from './base';

const widths = [320, 375, 768, 900, 1440];
const product = url('/#product');
const agents = url('/#agents');

test('no horizontal overflow at narrow and wide viewports', async ({ page }) => {
  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(url());
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    expect(overflow).toBeLessThanOrEqual(1);
  }
});

test('desktop navigation stays on one line where it is shown', async ({ page }) => {
  for (const width of [900, 1100, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(url());
    if (await page.locator('.nav-desktop').isVisible()) {
      const height = await page.locator('.nav-desktop .nav-links').evaluate((element) => {
        return element.getBoundingClientRect().height;
      });
      expect(height, `at ${width}px`).toBeLessThan(40);
    }
  }
});

test('mobile navigation opens and closes without JavaScript', async ({ browser }) => {
  const context = await browser.newContext({
    javaScriptEnabled: false,
    viewport: { width: 375, height: 900 },
  });
  const page = await context.newPage();
  await page.goto(url());
  const summary = page.locator('.nav-mobile summary');
  await expect(summary).toBeVisible();
  await summary.click();
  await expect(page.locator(`.nav-mobile a[href="${product}"]`)).toBeVisible();
  await expect(page.locator(`.nav-mobile a[href="${agents}"]`)).toBeVisible();
  await context.close();
});

test('anchor links clear the sticky header', async ({ page }) => {
  await page.goto(url());
  await page.locator(`.nav-desktop a[href="${product}"]`).click();
  await expect(page).toHaveURL(/#product$/);
  const top = await page.locator('#product').evaluate((element) => {
    return element.getBoundingClientRect().top;
  });
  expect(top).toBeGreaterThanOrEqual(72);

  await page.locator(`.nav-desktop a[href="${agents}"]`).click();
  await expect(page).toHaveURL(/#agents$/);
  const agentTop = await page.locator('#agents').evaluate((element) => {
    return element.getBoundingClientRect().top;
  });
  expect(agentTop).toBeGreaterThanOrEqual(72);
});

test('legal pages keep working primary navigation', async ({ page }) => {
  await page.goto(url('/privacy/'));
  await expect(page.locator('header.site-header')).toBeVisible();
  await page.locator(`.nav-desktop a[href="${product}"]`).click();
  await expect(page).toHaveURL(/\/#product$/);
  await expect(page.locator('#product')).toBeVisible();
});

test('technical accordions are keyboard operable', async ({ page }) => {
  await page.goto(url());
  const first = page.locator('#under-the-hood details').first();
  const summary = first.locator('summary');
  await summary.focus();
  await page.keyboard.press('Enter');
  await expect(first).toHaveAttribute('open', '');
});

test('JavaScript-disabled content is complete', async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto(url());
  await expect(page.locator('h1')).toHaveText('Get cooking without becoming a chef.');
  await expect(page.locator('#agents')).toBeVisible();
  await expect(page.locator('#support')).toBeVisible();
  await context.close();
});

test('reduced motion disables reveals and smooth scrolling', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto(url());
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

test('five taps on the hero image play the video, then restore the image', async ({ page }) => {
  await page.clock.install();
  await page.goto(url());
  await expect(page.locator('iframe')).toHaveCount(0);
  const egg = page.locator('[data-hero-egg]');
  for (let tap = 0; tap < 5; tap += 1) {
    await egg.click();
  }
  const frame = page.locator('.hero-video iframe');
  await expect(frame).toBeVisible();
  await expect(frame).toHaveAttribute('src', /youtube-nocookie\.com\/embed\/IRfgG3BtVlQ/);
  const src = await frame.getAttribute('src');
  expect(src).toContain('start=20');
  expect(src).toContain('end=90');
  await expect(page.locator('[data-hero-egg]')).toBeHidden();
  await expect(page.locator('[data-hero-caption]')).toHaveText(/Kernkraft 400/);

  await page.clock.fastForward(120_000);
  await expect(frame).toHaveCount(0);
  await expect(egg).toBeVisible();
  await expect(page.locator('[data-hero-caption]')).toHaveText(/single binary/);
});

test('page emits no console errors', async ({ page }) => {
  const errors: string[] = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  await page.goto(url());
  await page.waitForLoadState('networkidle');
  expect(errors).toEqual([]);
});
