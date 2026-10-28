# Homepage Steel Blue Palette and Flowing Grid — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the near-black homepage palette with a steel-blue ramp and add a faint, slowly flowing background grid on every page.

**Architecture:** Palette values become tokens in `tokens.css`; hard-coded warm grays collapse into a `--text-soft` token. A single decorative `GridField.astro` in `Base.astro` renders a fixed, `aria-hidden` plane of two gradient layers, moved with `transform` in a 24 s loop and frozen for reduced motion and narrow screens.

**Tech Stack:** Astro, vanilla CSS, Playwright, `node:test` + cheerio.

**Spec:** `docs/superpowers/specs/2026-10-28-homepage-steel-blue-grid-design.md`

## Global Constraints

- English copy only; no invented claims.
- No client-side framework, no new dependency, no runtime request.
- Site must work with JavaScript disabled.
- `prefers-reduced-motion: reduce` freezes the grid.
- Text contrast at least 4.5:1 on every surface it sits on.
- Run `npm test` in `website/homepage`; no commit before it is green.

## Review Focus

- Fixed layer with `z-index: -1` must stay behind content but above the body background on WebKit.
- Grid must never intercept clicks, focus, or text selection.
- Reduced motion and widths under 700 px must render a static grid, not a stopped-looking one.
- Focus ring (`--cyan`) must stay visible on the new blue.
- Long legal pages must stay readable under the grid.

---

### Task 1: Steel-blue palette tokens

**Files:**
- Modify: `website/homepage/src/styles/tokens.css:1-9`
- Modify: `website/homepage/src/styles/components.css` (hex grays at lines 232, 266, 396, 444, 488, 593, 719, 760, 802, 859, 926, 986, 1026, 1109, 1155; footer background at 1060)
- Modify: `website/homepage/src/styles/global.css:74-77` (selection pair)
- Modify: `website/homepage/src/layouts/Base.astro:56`
- Modify: `website/homepage/public/site.webmanifest:7-8`
- Modify: `website/homepage/public/favicon.svg:10`
- Test: `website/homepage/tests/e2e/palette.spec.ts` (create), `website/homepage/tests/content/seo.test.mjs:11`

**Interfaces:**
- Produces: CSS tokens `--ink #2a4d6e`, `--panel #2f5375`, `--panel-raised #345b80`, `--footer #24425f`, `--text #f7f5ef`, `--muted #c9d1dc`, `--text-soft #e8edf3`, `--line rgba(247, 245, 239, 0.16)`.

- [ ] **Step 1: Write the failing tests**

Create `website/homepage/tests/e2e/palette.spec.ts`:

```ts
import { test, expect } from '@playwright/test';

test('body and panels use the steel-blue ramp', async ({ page }) => {
  await page.goto('/');
  const body = page.locator('body');
  await expect(body).toHaveCSS('background-color', 'rgb(42, 77, 110)');
  await expect(page.locator('.gaps-section')).toHaveCSS(
    'background-color',
    'rgb(47, 83, 117)',
  );
});
```

Add to `website/homepage/tests/content/seo.test.mjs`:

```js
test('theme colour matches the steel-blue background', () => {
  assert.equal($('meta[name="theme-color"]').attr('content'), '#2a4d6e');
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd website/homepage && npx playwright test tests/e2e/palette.spec.ts && npm run test:content`
Expected: FAIL — background is `rgb(10, 10, 11)` and `theme-color` is `#0a0a0b`.

- [ ] **Step 3: Replace the tokens**

In `tokens.css`, set the block from line 2 to line 9 to:

```css
  color-scheme: dark;

  --ink: #2a4d6e;
  --panel: #2f5375;
  --panel-raised: #345b80;
  --footer: #24425f;
  --text: #f7f5ef;
  --muted: #c9d1dc;
  --text-soft: #e8edf3;
  --line: rgba(247, 245, 239, 0.16);
```

- [ ] **Step 4: Fold the hard-coded grays into tokens**

In `components.css`, replace every `#c9c6bd` and every `#d6d3ca` with `var(--text-soft)`, and the `.site-footer` background `#080809` with `var(--footer)`. Replace `rgba(244, 241, 232,` with `rgba(247, 245, 239,` on lines 169, 346, 560, 811.

In `global.css`, set `::selection` to `background: var(--text); color: var(--ink);`.

- [ ] **Step 5: Update metadata and the favicon tile**

`Base.astro`: `<meta name="theme-color" content="#2a4d6e" />`.
`site.webmanifest`: both colours to `#2a4d6e`.
`favicon.svg`: rect fill `#2a4d6e`.

- [ ] **Step 6: Re-run the tests**

Run: `cd website/homepage && npm test`
Expected: PASS, including axe on both widths. Inspect `test-results` screenshots when axe or contrast assertions complain.

- [ ] **Step 7: Commit**

```bash
git add website/homepage/src website/homepage/public website/homepage/tests
git commit -m "feat(website): move homepage palette to steel blue"
```

---

### Task 2: Flowing grid layer

**Files:**
- Create: `website/homepage/src/components/GridField.astro`
- Modify: `website/homepage/src/layouts/Base.astro:84-86`
- Modify: `website/homepage/src/styles/global.css` (append)
- Test: `website/homepage/tests/e2e/grid.spec.ts` (create), `website/homepage/tests/content/site.test.mjs:12`

**Interfaces:**
- Consumes: the palette tokens from Task 1.
- Produces: `.grid-field` root and `.grid-field__plane` child; no props.

- [ ] **Step 1: Write the failing tests**

Create `website/homepage/tests/e2e/grid.spec.ts`:

```ts
import { test, expect } from '@playwright/test';

test('grid sits behind content without taking clicks', async ({ page }) => {
  await page.goto('/');
  const field = page.locator('.grid-field');
  await expect(field).toHaveCount(1);
  await expect(field).toHaveAttribute('aria-hidden', 'true');
  await expect(field).toHaveCSS('pointer-events', 'none');
  await expect(field).toHaveCSS('position', 'fixed');
  await page.locator('h1').click();
  expect(await page.evaluate(() => document.elementFromPoint(4, 4)?.className)).not.toContain('grid-field');
});

test('grid holds still under reduced motion and on phones', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/');
  await expect(page.locator('.grid-field__plane')).toHaveCSS('animation-name', 'none');

  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await page.setViewportSize({ width: 375, height: 800 });
  await page.goto('/');
  await expect(page.locator('.grid-field__plane')).toHaveCSS('animation-name', 'none');
});

test('grid runs on desktop with motion allowed', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/');
  await expect(page.locator('.grid-field__plane')).toHaveCSS('animation-name', 'grid-flow');
  await expect(page.locator('.grid-field__plane')).toHaveCSS('opacity', '0.18');
});
```

Add to `website/homepage/tests/content/site.test.mjs`:

```js
test('every page carries exactly one decorative grid layer', () => {
  assert.equal($('.grid-field').length, 1);
  for (const name of ['privacy.html', 'imprint.html', '404.html']) {
    const page = cheerio.load(fs.readFileSync(path.join(root, 'dist', name), 'utf8'));
    assert.equal(page('.grid-field[aria-hidden="true"]').length, 1, name);
  }
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd website/homepage && npx playwright test tests/e2e/grid.spec.ts && npm run test:content`
Expected: FAIL — no `.grid-field` in the DOM.

- [ ] **Step 3: Add the component**

`src/components/GridField.astro`:

```astro
<div class="grid-field" aria-hidden="true">
  <div class="grid-field__plane"></div>
</div>
```

- [ ] **Step 4: Render it from the layout**

In `Base.astro`, place `<GridField />` directly after `<body>` and add the import next to the other component import.

- [ ] **Step 5: Add the styles to `global.css`**

```css
.grid-field {
  position: fixed;
  inset: 0;
  z-index: -1;
  overflow: hidden;
  pointer-events: none;
  mask-image: linear-gradient(to bottom, transparent, #000 15%, #000 85%, transparent);
}

.grid-field__plane {
  position: absolute;
  inset: -30% -25%;
  background-image:
    linear-gradient(rgba(61, 220, 255, 0.5) 1px, transparent 1px),
    linear-gradient(90deg, rgba(61, 220, 255, 0.35) 1px, transparent 1px);
  background-size: 48px 48px;
  opacity: 0.18;
  transform-origin: 50% 0;
  animation: grid-flow 24s linear infinite;
}

@keyframes grid-flow {
  from {
    transform: perspective(420px) rotateX(63deg) translateY(0);
  }

  to {
    transform: perspective(420px) rotateX(63deg) translateY(48px);
  }
}

@media (max-width: 699px) {
  .grid-field__plane {
    opacity: 0.12;
    animation: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  .grid-field__plane {
    animation: none;
  }
}
```

The `@media` rules must sit after the animation declaration so `animation: none` wins.

- [ ] **Step 6: Re-run the tests and inspect**

Run: `cd website/homepage && npm test`
Expected: PASS. Then `npm run dev`, check 320/375/768/900/1440 px by eye: no horizontal scrollbar, text in code blocks and the legal pages stays readable, focus rings visible.

- [ ] **Step 7: Commit**

```bash
git add website/homepage/src/components/GridField.astro website/homepage/src/layouts/Base.astro website/homepage/src/styles/global.css website/homepage/tests
git commit -m "feat(website): add flowing steel-blue background grid"
```
