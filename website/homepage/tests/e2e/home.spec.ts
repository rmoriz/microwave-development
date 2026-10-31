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
  expect(src).toContain('start=21');
  expect(src).toContain('end=90');
  await expect(page.locator('[data-hero-egg]')).toBeHidden();
  await expect(page.locator('[data-hero-caption]')).toHaveText(/Kernkraft 400/);

  await page.clock.fastForward(120_000);
  await expect(frame).toHaveCount(0);
  await expect(egg).toBeVisible();
  await expect(page.locator('[data-hero-caption]')).toHaveText(/single binary/);
});

test('scrolling the video out of view restores the image', async ({ page }) => {
  await page.goto(url());
  const egg = page.locator('[data-hero-egg]');
  for (let tap = 0; tap < 5; tap += 1) {
    await egg.click();
  }
  await expect(page.locator('.hero-video iframe')).toBeVisible();
  await page.locator('#faq').scrollIntoViewIfNeeded();
  await expect(page.locator('.hero-video iframe')).toHaveCount(0);
  await expect(egg).toBeVisible();
});

test('the MCP demo loads only on click and plays the recording', async ({ page }) => {
  const requests: string[] = [];
  page.on('request', (request) => requests.push(request.url()));
  await page.goto(url());
  await expect(page.locator('.mcp-demo .ap-player')).toHaveCount(0);
  expect(requests.some((request) => request.includes('/demo/mcp-agent.cast'))).toBe(false);

  const play = page.locator('[data-mcp-demo-play]');
  await expect(play).toBeVisible();
  await play.click();
  await expect(page.locator('.mcp-demo .ap-player')).toBeVisible();
  await expect(play).toHaveCount(0);
  await expect(page.locator('.mcp-demo .ap-term-text')).toBeVisible();
  expect(requests.filter((request) => request.includes('/demo/mcp-agent.cast')).length).toBe(1);
  await expect(page.locator('.mcp-demo .ap-playback-button')).toHaveAttribute('aria-label', 'Pause');
});

test('the demo shows the whole recording on a phone in portrait', async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await page.goto(url());
  const play = page.locator('[data-mcp-demo-play]');
  await play.scrollIntoViewIfNeeded();
  await play.click();
  const term = page.locator('.mcp-demo .ap-term');
  await expect(term).toBeVisible();

  const fit = await page.evaluate(() => {
    const stage = document.querySelector('[data-mcp-demo]')!;
    const terminal = stage.querySelector('.ap-term')!;
    const stageBox = stage.getBoundingClientRect();
    const termBox = terminal.getBoundingClientRect();
    return {
      clippedBelow: termBox.bottom - stageBox.bottom,
      clippedRight: termBox.right - stageBox.right,
      hiddenRows: terminal.scrollHeight - terminal.clientHeight,
      pageOverflow: document.documentElement.scrollWidth - window.innerWidth,
    };
  });

  expect(fit.clippedBelow).toBeLessThanOrEqual(1);
  expect(fit.clippedRight).toBeLessThanOrEqual(1);
  expect(fit.hiddenRows).toBeLessThanOrEqual(1);
  expect(fit.pageOverflow).toBeLessThanOrEqual(1);
});

test('the hero video fills its frame instead of letterboxing', async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await page.goto(url());
  const egg = page.locator('[data-hero-egg]');
  for (let tap = 0; tap < 5; tap += 1) {
    await egg.click();
  }
  const frame = page.locator('.hero-video iframe');
  await expect(frame).toBeVisible();

  const fit = await page.evaluate(() => {
    const box = document.querySelector('[data-hero-video]')!;
    const embed = box.querySelector('iframe')!;
    const outer = box.getBoundingClientRect();
    const inner = embed.getBoundingClientRect();
    return {
      height: Math.round(inner.height - outer.height),
      width: Math.round(inner.width - outer.width),
      ratio: inner.width / inner.height,
      overflow: document.documentElement.scrollWidth - window.innerWidth,
    };
  });

  // The 1px border aside, the embed has to cover the box: no bars above or
  // below, and no crop.
  expect(Math.abs(fit.height)).toBeLessThanOrEqual(3);
  expect(Math.abs(fit.width)).toBeLessThanOrEqual(3);
  expect(fit.ratio).toBeCloseTo(16 / 9, 1);
  expect(fit.overflow).toBeLessThanOrEqual(1);
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
