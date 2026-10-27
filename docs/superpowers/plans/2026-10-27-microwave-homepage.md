# Microwave Homepage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the static microwave.io landing page in `website/homepage` — a sales-grade page with enough technical evidence for an engineer to start an evaluation.

**Architecture:** Astro 7 static site with vanilla CSS and a tiny progressive-enhancement script. Content is composed from one page into focused section components; no UI framework, no runtime server, no third-party requests. Tests parse the built HTML with Node's test runner and exercise interactions with Playwright.

**Tech Stack:** Astro 7, TypeScript, vanilla CSS, `@fontsource-variable/*`, `sharp`, Node test runner, Cheerio, Playwright, axe-core.

**Spec:** `docs/superpowers/specs/2026-10-27-microwave-homepage-design.md`

## Global Constraints

- Every file lives under `website/homepage/` unless this plan says otherwise.
- Static output only. No server runtime, no database, no form backend, no analytics.
- No third-party runtime requests. Fonts and images are self-hosted; no Google Fonts, no CDNs, no tracking.
- No install command anywhere on the page. The secondary CTA “Run it from source” links to `https://github.com/rmoriz/microwave-development#run-locally`.
- Contact is `hello@microwave.io` via `mailto:`.
- Do not disparage Chef, Progress Software, or the Cinc Project. No pricing attacks, no “cheaper than”, no “drop the license”, no EOL fear, no “official partner”, no implied endorsement.
- Chef, Chef Infra, and Chef Infra Server are trademarks of Progress Software Corporation. Cinc is a separate project. The footer disclaimer is mandatory.
- Microwave Server is AGPL-3.0-only. Upstream source snapshots retain Apache-2.0.
- Cinc may only be described as “tested” after Task 10 runs the Compose stack. Until then use “ships an end-to-end Compose stack covering…”.
- No invented customers, testimonials, logos, benchmarks, prices, SLAs, or response times.
- WCAG 2.2 AA, keyboard operable, `prefers-reduced-motion` respected, content usable with JavaScript disabled.
- Lighthouse 95+ in all four categories on the production build.
- Initial page weight under 1MB; hero image under 300KB for the largest variant.
- English copy.
- Operator legal details are placeholders; the site is not launch-ready until the owner supplies them.

## Review Focus

- **JavaScript disabled:** every section, the mobile navigation, and every accordion must still work. Owning test: Task 7.
- **Narrow viewport (320px):** no horizontal overflow, clipped headline, or unreachable navigation. Owning test: Task 7.
- **Keyboard and screen reader:** skip link, nav toggle, accordions, and CTAs must all be operable and announced. Owning test: Task 8.
- **Reduced motion:** no ambient float, scroll reveal, or smooth scrolling when the user asks for less motion. Owning test: Task 7.
- **Claim and legal integrity:** forbidden competitor language, missing disclaimers, unverified Cinc wording, or dead legal links must fail the build tests. Owning tests: Tasks 6 and 10.

---

### Task 1: Scaffold the Astro site and test harness

**Files:**
- Create: `website/homepage/package.json`
- Create: `website/homepage/astro.config.mjs`
- Create: `website/homepage/tsconfig.json`
- Create: `website/homepage/.gitignore`
- Create: `website/homepage/public/favicon.svg`
- Create: `website/homepage/src/data/site.ts`
- Create: `website/homepage/src/styles/tokens.css`
- Create: `website/homepage/src/styles/global.css`
- Create: `website/homepage/src/layouts/Base.astro`
- Create: `website/homepage/src/pages/index.astro`
- Test: `website/homepage/tests/content/site.test.mjs`

**Interfaces:**
- Consumes: nothing.
- Produces: `site` object from `src/data/site.ts` with `name`, `url`, `title`, `description`, `email`, `github`, `runFromSource`, `docs`, `license`, `cinc`, and `legal` (`operator`, `address`, `email`).
- Produces: `Base.astro` props `{ title: string; description: string; image?: string; noindex?: boolean }`, rendering `<html>`, `<head>`, a skip link, a `header` slot, and `<main id="main">`.
- Produces: CSS tokens `--ink`, `--panel`, `--text`, `--muted`, `--line`, `--gradient`, `--font-display`, `--font-body`, `--font-mono`.

- [ ] **Step 1: Write the failing scaffold test**

Create `tests/content/site.test.mjs`:

```js
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as cheerio from 'cheerio';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const html = fs.readFileSync(path.join(root, 'dist/index.html'), 'utf8');
const $ = cheerio.load(html);

test('page shell: title, description, skip link, main landmark', () => {
  assert.equal($('title').text(), 'Microwave Server — Get cooking without becoming a chef.');
  assert.match($('meta[name="description"]').attr('content'), /independent, open-source Chef Infra Server API implementation in Rust/i);
  assert.equal($('a.skip-link').attr('href'), '#main');
  assert.equal($('main#main').length, 1);
});
```

- [ ] **Step 2: Create the package manifest**

`package.json`:

```json
{
  "name": "microwave-homepage",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "astro dev",
    "build": "astro build",
    "preview": "astro preview",
    "check": "astro check",
    "test:content": "npm run build && node --test tests/content/",
    "test:e2e": "playwright test",
    "test": "npm run check && npm run test:content && npm run test:e2e",
    "og": "node scripts/generate-og.mjs"
  },
  "dependencies": {
    "@astrojs/sitemap": "^3.7.4",
    "@fontsource-variable/inter": "^5.3.0",
    "@fontsource-variable/jetbrains-mono": "^5.3.0",
    "@fontsource-variable/space-grotesk": "^5.3.0",
    "astro": "^7.4.2"
  },
  "devDependencies": {
    "@astrojs/check": "^0.9.12",
    "@axe-core/playwright": "^4.14.0",
    "@playwright/test": "^1.64.1",
    "cheerio": "^1.2.0",
    "sharp": "^0.35.5",
    "typescript": "^6.0.3"
  }
}
```

- [ ] **Step 3: Install dependencies**

Run: `npm install`
Expected: `package-lock.json` is created; no install errors.

- [ ] **Step 4: Run the test to verify it fails**

Run: `npm run test:content`
Expected: FAIL because `astro.config.mjs` and the page do not exist.

- [ ] **Step 5: Implement the scaffold**

Create `astro.config.mjs`:

```js
import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

export default defineConfig({
  site: 'https://microwave.io',
  integrations: [sitemap()],
  build: { inlineStylesheets: 'auto' },
});
```

Create `tsconfig.json` extending `astro/tsconfigs/strict`.

Create `.gitignore` with `node_modules/`, `dist/`, `.astro/`, `test-results/`, `playwright-report/`.

Create `src/data/site.ts`:

```ts
export const site = {
  name: 'Microwave Server',
  url: 'https://microwave.io',
  title: 'Microwave Server — Get cooking without becoming a chef.',
  description:
    'Microwave Server is an independent, open-source Chef Infra Server API implementation in Rust. Works with Cinc Client, keeps your cookbooks and CLI, and is free to self-host under AGPL-3.0.',
  email: 'hello@microwave.io',
  github: 'https://github.com/rmoriz/microwave-development',
  runFromSource: 'https://github.com/rmoriz/microwave-development#run-locally',
  docs: 'https://github.com/rmoriz/microwave-development/tree/main/docs',
  license: 'https://github.com/rmoriz/microwave-development/blob/main/LICENSE',
  cinc: 'https://cinc.sh/',
  legal: {
    operator: '',
    address: '',
    email: 'hello@microwave.io',
  },
};
```

Create `src/styles/tokens.css` with the palette and typography from the spec:
`--ink: #0a0a0b`, `--panel: #121214`, `--text: #f4f1e8`, `--muted: #a3a39b`,
`--line: rgba(244, 241, 232, 0.12)`, the six gradient stops, and the three
font families.

Create `src/styles/global.css` with a modern reset, `box-sizing: border-box`,
body colors, link styles, focus-visible ring, `.skip-link` (visually hidden
until focused), `.container` (max 1200px, 24px gutters), and
`@media (prefers-reduced-motion: reduce)` disabling animations and smooth
scrolling.

Create `public/favicon.svg` with a simple rounded-square microwave glyph and a
solid accent fallback. Task 9 may refine it, but the file must exist so the
early Playwright console test does not see a missing-favicon request.

Create `src/layouts/Base.astro`:

```astro
---
import '@fontsource-variable/inter';
import '@fontsource-variable/jetbrains-mono';
import '@fontsource-variable/space-grotesk';
import '../styles/tokens.css';
import '../styles/global.css';

interface Props {
  title: string;
  description: string;
  image?: string;
  noindex?: boolean;
}

const { title, description, image = '/og.png', noindex = false } = Astro.props;
---
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
    <meta name="description" content={description} />
    <link rel="canonical" href={new URL(Astro.url.pathname, Astro.site)} />
    <meta property="og:title" content={title} />
    <meta property="og:description" content={description} />
    <meta property="og:type" content="website" />
    <meta property="og:url" content={new URL(Astro.url.pathname, Astro.site)} />
    <meta property="og:image" content={new URL(image, Astro.site)} />
    <meta name="twitter:card" content="summary_large_image" />
    {noindex && <meta name="robots" content="noindex" />}
  </head>
  <body>
    <a class="skip-link" href="#main">Skip to content</a>
    <slot name="header" />
    <main id="main"><slot /></main>
  </body>
</html>
```

`src/pages/index.astro` passes `site.title` and `site.description` to the
layout and renders a temporary `<h1>` so the shell test can pass. Task 2
replaces it with the hero.

- [ ] **Step 6: Run the test to verify it passes**

Run: `npm run test:content`
Expected: PASS, 1 test.

- [ ] **Step 7: Commit**

```bash
git add website/homepage
git commit -m "feat(website): scaffold microwave homepage"
```

---

### Task 2: Navigation and hero

**Files:**
- Create: `website/homepage/src/assets/microwave-hero.png`
- Create: `website/homepage/src/components/Logo.astro`
- Create: `website/homepage/src/components/Nav.astro`
- Create: `website/homepage/src/components/Hero.astro`
- Modify: `website/homepage/src/pages/index.astro`
- Modify: `website/homepage/src/styles/global.css`
- Create: `website/homepage/src/styles/components.css`
- Test: `website/homepage/tests/content/site.test.mjs`

**Interfaces:**
- Consumes: `site` from Task 1; `Base.astro` from Task 1.
- Produces: `Logo.astro` with no props, rendering an inline SVG wordmark.
- Produces: `Nav.astro` with no props, containing desktop links (`#product`, `#compatibility`, `#under-the-hood`, `#support`, `#faq`), a GitHub link, a “Talk to us” mailto CTA, and a mobile `<details>` menu.
- Produces: `Hero.astro` with no props, importing `microwave-hero.png` through `astro:assets`.

- [ ] **Step 1: Add the failing hero test**

Append to `tests/content/site.test.mjs`:

```js
test('hero and navigation', () => {
  assert.equal($('h1').text().trim(), 'Get cooking without becoming a chef.');
  assert.match($('main').text(), /independent, open-source Chef Infra Server API implementation written in Rust/i);
  assert.equal($('a[href="mailto:hello@microwave.io?subject=Microwave%20support"]').length >= 1, true);
  assert.equal($('a[href="https://github.com/rmoriz/microwave-development#run-locally"]').length >= 1, true);
  assert.match($('body').text(), /Cinc Client 19/);
  assert.match($('body').text(), /Chef Infra Client 18\/19/);
  assert.match($('body').text(), /AGPL-3\.0/);
  assert.match($('picture img').attr('alt'), /rainbow neon microwave/i);
  assert.equal($('body').text().includes('docker run'), false);
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `node --test --test-name-pattern="hero and navigation" tests/content/site.test.mjs`
Expected: FAIL on the `h1` text.

- [ ] **Step 3: Add the hero asset**

Copy `docs/images/microwave-hero.png` to
`website/homepage/src/assets/microwave-hero.png`.

- [ ] **Step 4: Implement `Logo`, `Nav`, and `Hero`**

`Logo.astro`: an inline SVG microwave-window glyph plus the lowercase wordmark
`microwave` in the display font. The glyph uses the gradient stops but must
remain legible if the gradient fails; include a solid fallback stroke.

`Nav.astro`: sticky header, `.container`, desktop `<ul>` with the five anchor
links, GitHub link, and a primary “Talk to us” mailto button. On mobile, a
`<details class="nav-mobile">` with a `<summary aria-label="Open menu">` and the
same links. The desktop list is hidden below 800px; the details menu is hidden
above 800px. No JavaScript is required.

`Hero.astro`: eyebrow, exact `h1`, subhead from the spec, primary CTA
“Talk to us about support” (`mailto:hello@microwave.io?subject=Microwave%20support`),
secondary CTA “Run it from source” (`site.runFromSource`), three badges, and a
`<Picture>` from `astro:assets` with `formats={['avif', 'webp']}`,
`widths={[640, 960, 1280, 1536]}`, `sizes="(min-width: 900px) 46vw, 100vw"`,
and `alt="A rainbow neon microwave glowing on a dark surface"`. Add the caption
“Artist's impression. The real thing is a single binary.”

Add the section styles to `src/styles/components.css` and import it in
`Base.astro` after `global.css`. Keep the visual work deliberate but minimal in
this task; Task 7 handles responsive polish.

- [ ] **Step 5: Run the test to verify it passes**

Run: `node --test --test-name-pattern="hero and navigation" tests/content/site.test.mjs`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add website/homepage
git commit -m "feat(website): add navigation and hero"
```

---

### Task 3: Proof strip and Cinc section

**Files:**
- Create: `website/homepage/src/components/ProofStrip.astro`
- Create: `website/homepage/src/components/CincSection.astro`
- Modify: `website/homepage/src/pages/index.astro`
- Modify: `website/homepage/src/styles/components.css`
- Test: `website/homepage/tests/content/site.test.mjs`

**Interfaces:**
- Consumes: `site` from Task 1.
- Produces: `ProofStrip.astro` and `CincSection.astro`, both with no props.
- Produces: section ids `#product` (Cinc section) and `#compatibility` is added in Task 4.

- [ ] **Step 1: Add the failing proof/Cinc test**

Append:

```js
test('proof strip and Cinc section', () => {
  const text = $('body').text();
  assert.match(text, /3,284/);
  assert.match(text, /extended Pedant examples passing/);
  assert.match(text, /compatibility shims/);
  assert.match(text, /Debian, Ubuntu, and Fedora/);
  assert.match(text, /Cinc Client 19/);
  assert.match(text, /ships an end-to-end Compose stack|tested with Cinc Client 19/i);
  assert.equal($('a[href="https://cinc.sh/"]').length >= 1, true);
  assert.match(text, /not affiliated with or endorsed by the Cinc Project/i);
  assert.doesNotMatch(text, /official partner/i);
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `node --test --test-name-pattern="proof strip and Cinc section" tests/content/site.test.mjs`
Expected: FAIL because the sections do not exist.

- [ ] **Step 3: Implement the two sections**

`ProofStrip.astro`: four stat cells.

| Number | Label | Link |
| --- | --- | --- |
| 3,284 | extended Pedant examples passing, 0 failures | `https://github.com/rmoriz/microwave-development/blob/main/docs/pedant-extended-report.md` |
| 0 | compatibility shims needed for the strict `chef-client` converge | `https://github.com/rmoriz/microwave-development/blob/main/docs/chef-client-e2e-report.md` |
| 3 | distros in the Cinc Client 19 Compose e2e stack | `https://github.com/rmoriz/microwave-development/tree/main/docker-e2e` |
| 1 | binary to run; SQLite by default | `https://github.com/rmoriz/microwave-development#readme` |

`CincSection.astro` with `id="product"`:

- Heading: “Cinc Client, meet Microwave.”
- Copy: Cinc gives teams a free, community-built client; Microwave gives it an
  independent server. Keep cookbooks, roles, data bags, search, policyfiles,
  and pipelines.
- Conservative compatibility line until Task 10: “The repository ships an
  end-to-end Compose stack covering Debian, Ubuntu, and Fedora with Cinc Client
  19.”
- Link to `site.cinc`.
- Small print: “Cinc is a separate project. Microwave is not affiliated with or
  endorsed by the Cinc Project.”

- [ ] **Step 4: Run the test to verify it passes**

Run: `node --test --test-name-pattern="proof strip and Cinc section" tests/content/site.test.mjs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add website/homepage
git commit -m "feat(website): add proof strip and Cinc section"
```

---

### Task 4: Compatibility, under-the-hood, and known gaps

**Files:**
- Create: `website/homepage/src/components/Compatibility.astro`
- Create: `website/homepage/src/components/UnderTheHood.astro`
- Create: `website/homepage/src/components/KnownGaps.astro`
- Modify: `website/homepage/src/pages/index.astro`
- Modify: `website/homepage/src/styles/components.css`
- Test: `website/homepage/tests/content/site.test.mjs`

**Interfaces:**
- Consumes: `site.docs`, `site.github` from Task 1.
- Produces: section id `#compatibility` and `#under-the-hood`.
- Produces: `UnderTheHood.astro` rendering exactly six native `<details>` panels.

- [ ] **Step 1: Add the failing technical-section test**

Append:

```js
test('compatibility, under the hood, and known gaps', () => {
  const text = $('body').text();
  assert.equal($('#compatibility').length, 1);
  assert.equal($('#under-the-hood').length, 1);
  assert.match(text, /Same recipe\. New kitchen\./);
  assert.match(text, /X-Ops-Sign/);
  assert.match(text, /knife/);
  assert.match(text, /chef-server-ctl/);
  assert.equal($('#under-the-hood details').length, 6);
  for (const phrase of [
    'No automatic `chef-server.rb` importer',
    'password reset',
    'No LDAP/Active Directory bind',
    'Multi-replica HA and automated failover are not validated',
    'relational scan',
  ]) {
    assert.equal(text.includes(phrase.replaceAll('`', '')), true, phrase);
  }
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `node --test --test-name-pattern="compatibility, under the hood, and known gaps" tests/content/site.test.mjs`
Expected: FAIL because the sections do not exist.

- [ ] **Step 3: Implement the three sections**

`Compatibility.astro` with `id="compatibility"`: heading “Same recipe. New
kitchen.”, three columns (API, Auth, Tooling) with the lists from the spec, and
a link to the README HTTP route parity section and the Pedant reports.

`UnderTheHood.astro` with `id="under-the-hood"`: heading “Under the hood.” and
six `<details>` panels, each with a `<summary>` and two or three short
paragraphs plus a repository link:

1. “One binary, two databases.”
2. “Auth that matches.”
3. “Cookbooks and policy.”
4. “Search your way.”
5. “Deploy your way.” — include the honest sentence “Documented topologies are
   single-node or external-backend; multi-replica HA is not yet validated.”
6. “Move in.” — include “There is no automatic `chef-server.rb` importer.”

`KnownGaps.astro`: heading “What is not done yet.” and the six-item list from
the spec §8.5.1. Link to the README “Remaining Work” section.

- [ ] **Step 4: Run the test to verify it passes**

Run: `node --test --test-name-pattern="compatibility, under the hood, and known gaps" tests/content/site.test.mjs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add website/homepage
git commit -m "feat(website): add compatibility and technical detail"
```

---

### Task 5: Migration, support, and FAQ

**Files:**
- Create: `website/homepage/src/components/Migration.astro`
- Create: `website/homepage/src/components/Support.astro`
- Create: `website/homepage/src/components/FAQ.astro`
- Modify: `website/homepage/src/pages/index.astro`
- Modify: `website/homepage/src/styles/components.css`
- Test: `website/homepage/tests/content/site.test.mjs`

**Interfaces:**
- Consumes: `site.email`, `site.docs` from Task 1.
- Produces: section ids `#support` and `#faq`.
- Produces: `FAQ.astro` rendering exactly eight native `<details>` items.

- [ ] **Step 1: Add the failing migration/support/FAQ test**

Append:

```js
test('migration, support, and FAQ', () => {
  const text = $('body').text();
  assert.match(text, /knife ec backup/i);
  assert.match(text, /from-api/);
  assert.match(text, /cannot copy password hashes or the admin flag/i);
  assert.match(text, /migration help/i);
  assert.match(text, /deployment review/i);
  assert.match(text, /ongoing support/i);
  assert.equal($('#support').length, 1);
  assert.equal($('#faq').length, 1);
  assert.equal($('#faq details').length, 8);
  assert.equal($('a[href="mailto:hello@microwave.io?subject=Microwave%20support"]').length >= 2, true);
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `node --test --test-name-pattern="migration, support, and FAQ" tests/content/site.test.mjs`
Expected: FAIL because the sections do not exist.

- [ ] **Step 3: Implement the three sections**

`Migration.astro`: heading “From your current server.” Two clearly labelled
paths:

- **With a `knife ec backup`** — `import-knife-backup`; preserves users,
  password hashes when the key dump contains them, organizations, clients,
  ACLs, cookbooks, and local cookbook blobs. Note the things a knife backup
  does not contain.
- **With only the live server** — `from-api`; reads the listed objects, but the
  legacy API does not expose password hashes or the admin flag, so those users
  need a password reset or external identity.

Then “Repoint clients and DNS at your pace.” Note that snapshot export/import
is for Microwave-to-Microwave moves. Link to `site.docs`.

`Support.astro` with `id="support"`: heading “Free software. Real support.”
Copy: AGPL-3.0-only, free to self-host, no seat count or licence fee.
Commercial support is available for migration, deployment review, and ongoing
support; scope and terms are agreed directly. Three cards with those labels and
one-sentence descriptions. No prices, tiers, SLAs, or response times. Primary
mailto CTA.

`FAQ.astro` with `id="faq"`: heading “Questions your boss will ask.” and eight
`<details>` items matching spec §8.9. Answers must be direct; the
production-readiness answer must mention evaluating against the customer's own
runbooks and documented gaps, not a blanket “yes”.

- [ ] **Step 4: Run the test to verify it passes**

Run: `node --test --test-name-pattern="migration, support, and FAQ" tests/content/site.test.mjs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add website/homepage
git commit -m "feat(website): add migration, support, and FAQ"
```

---

### Task 6: Final CTA, footer, legal pages, and link integrity

**Files:**
- Create: `website/homepage/src/components/FinalCTA.astro`
- Create: `website/homepage/src/components/Footer.astro`
- Create: `website/homepage/src/pages/privacy.astro`
- Create: `website/homepage/src/pages/imprint.astro`
- Modify: `website/homepage/src/pages/index.astro`
- Modify: `website/homepage/src/layouts/Base.astro`
- Modify: `website/homepage/src/styles/components.css`
- Test: `website/homepage/tests/content/site.test.mjs`
- Test: `website/homepage/tests/content/links.test.mjs`

**Interfaces:**
- Consumes: `site` from Task 1.
- Produces: `Footer.astro` with the mandatory disclaimer and legal links.
- Produces: `/privacy/` and `/imprint/` static pages.
- Produces: link-integrity test covering every page in `dist/`.

- [ ] **Step 1: Add the failing footer/legal test**

Append to `site.test.mjs`:

```js
test('final CTA, footer, and legal pages', () => {
  const text = $('body').text();
  assert.match(text, /Bring your own cookbooks\./);
  assert.match(text, /not affiliated with or endorsed by Progress Software Corporation/i);
  assert.match(text, /trademarks of Progress Software Corporation/i);
  assert.match(text, /AGPL-3\.0-only/);
  assert.match(text, /No cookies, no analytics, no third-party requests/i);
  assert.equal($('a[href="/privacy/"]').length, 1);
  assert.equal($('a[href="/imprint/"]').length, 1);
  assert.doesNotMatch(text, /cheaper than|drop the license|end of life|official partner/i);

  for (const page of ['privacy', 'imprint']) {
    const pageHtml = fs.readFileSync(path.join(root, `dist/${page}/index.html`), 'utf8');
    assert.match(pageHtml, /hello@microwave\.io/);
  }
});
```

- [ ] **Step 2: Run it to verify it fails**

Run:

```bash
npm run build
node --test --test-name-pattern="final CTA, footer, and legal pages" tests/content/site.test.mjs
```

Expected: FAIL because the footer and pages do not exist.

- [ ] **Step 3: Implement the final CTA, footer, and legal pages**

`FinalCTA.astro`: heading “Bring your own cookbooks.”, “Run it from source”
link, “Talk to us” mailto, GitHub link, and the line “Free to self-host.
Supported when you want it.”

`Footer.astro`:

- Independence and trademark disclaimer, exact text:
  “Microwave Server is an independent project. It is not affiliated with or
  endorsed by Progress Software Corporation, Chef Software, or the Cinc
  Project. Chef, Chef Infra, and Chef Infra Server are trademarks of Progress
  Software Corporation. Cinc is a project of the Cinc community. Microwave
  Server is licensed under AGPL-3.0-only; vendored upstream source snapshots
  retain their original Apache-2.0 licensing.”
- Links: GitHub, docs, license, privacy, imprint.
- Privacy line: “No cookies, no analytics, no third-party requests.”
- Copyright `© 2026 Microwave Server`.

`privacy.astro`: title “Privacy”, description of the no-cookie/no-analytics
policy, the hosting provider's standard server logs caveat, the contact email,
and a link back home. Do not claim a specific retention period or provider.

`imprint.astro`: title “Imprint”, operator name/address/contact from
`site.legal`; when a field is empty, render `[to be supplied]` and a visible
line “This page must be completed before launch.” The page must never invent a
legal entity or address.

Import `Footer.astro` in `Base.astro` and render it after `<main>`, so the
homepage, legal pages, and 404 all carry the disclaimer. The homepage does not
import the footer separately.

- [ ] **Step 4: Write the link-integrity test**

Create `tests/content/links.test.mjs` with a recursive scan of `dist/**/*.html`:

- Every `href` starting with `/` must resolve to a file
  (`/` → `index.html`, `/privacy/` → `privacy/index.html`).
- Every fragment (`/#faq`, `/imprint/#contact`) must match an `id` on the
  target page.
- Every external link must start with `https://` or `mailto:`.
- No empty `href`, `href="#"`, or `javascript:` URLs.
- No `http://` links.

- [ ] **Step 5: Run both tests to verify they pass**

Run: `npm run test:content`
Expected: PASS, all content tests.

- [ ] **Step 6: Commit**

```bash
git add website/homepage
git commit -m "feat(website): add final CTA, footer, and legal pages"
```

---

### Task 7: Responsive behavior, interactions, and motion

**Files:**
- Modify: `website/homepage/src/styles/components.css`
- Modify: `website/homepage/src/styles/global.css`
- Modify: `website/homepage/src/layouts/Base.astro`
- Create: `website/homepage/playwright.config.ts`
- Test: `website/homepage/tests/e2e/home.spec.ts`

**Interfaces:**
- Consumes: the page from Tasks 1–6.
- Produces: a `<script>` in `Base.astro` that adds `js` to `<html>`, sets up
  `IntersectionObserver` reveals, and removes the `js` class if it fails.
- Produces: `npm run test:e2e` running against `npm run preview` on port 4321.

- [ ] **Step 1: Create the Playwright config**

`playwright.config.ts`:

```ts
import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: true,
  use: {
    baseURL: 'http://127.0.0.1:4321',
    trace: 'retain-on-failure',
  },
  webServer: {
    command: 'npm run build && npm run preview -- --host 127.0.0.1 --port 4321',
    url: 'http://127.0.0.1:4321',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
});
```

Run: `npx playwright install chromium`
Expected: Chromium downloaded.

- [ ] **Step 2: Write the failing interaction tests**

`tests/e2e/home.spec.ts`:

```ts
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

test('mobile navigation opens and closes without JavaScript', async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 900 });
  await page.setJavaScriptEnabled(false);
  await page.goto('/');
  const summary = page.locator('.nav-mobile summary');
  await expect(summary).toBeVisible();
  await summary.click();
  await expect(page.locator('.nav-mobile a[href="#product"]')).toBeVisible();
});

test('technical accordions are keyboard operable', async ({ page }) => {
  await page.goto('/');
  const first = page.locator('#under-the-hood details').first();
  const summary = first.locator('summary');
  await summary.focus();
  await page.keyboard.press('Enter');
  await expect(first).toHaveAttribute('open', '');
});

test('JavaScript-disabled content is complete', async ({ page }) => {
  await page.setJavaScriptEnabled(false);
  await page.goto('/');
  await expect(page.locator('h1')).toHaveText('Get cooking without becoming a chef.');
  await expect(page.locator('#support')).toBeVisible();
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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `npm run test:e2e`
Expected: at least the overflow, mobile-nav, and reduced-motion tests fail.

- [ ] **Step 4: Implement the responsive CSS and progressive enhancement**

In `components.css`:

- Hero grid switches to one column below 900px; h1 uses
  `clamp(3rem, 8vw, 7rem)` and must not overflow at 320px.
- Nav desktop list hides below 800px; mobile `<details>` appears. Style the
  summary as a button and the open panel as a full-width sheet.
- Proof strip wraps from four columns to two to one.
- Compatibility, support, and step cards collapse to one column.
- Accordion and FAQ summaries show a plus/minus indicator and a clear focus
  ring.
- Section spacing 96px mobile, 128–160px desktop.
- The rainbow gradient appears only on thin rules, badge borders, the logo,
  and small highlights — not as a full background.

In `Base.astro`, add an inline script:

```ts
document.documentElement.classList.add('js');
try {
  const reveals = document.querySelectorAll('.reveal');
  if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
    reveals.forEach((el) => el.classList.add('is-visible'));
  } else {
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.classList.add('is-visible');
          observer.unobserve(entry.target);
        }
      }
    }, { rootMargin: '0px 0px -10% 0px' });
    reveals.forEach((el) => observer.observe(el));
  }
} catch {
  document.documentElement.classList.remove('js');
}
```

In CSS, `.reveal` is visible by default; `.js .reveal` starts with
`opacity: 0; transform: translateY(12px)` and `.js .reveal.is-visible` returns
to normal. Under reduced motion, remove the transform and animation.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `npm run test:e2e`
Expected: PASS, all interaction tests.

- [ ] **Step 6: Commit**

```bash
git add website/homepage
git commit -m "feat(website): add responsive interactions and motion"
```

---

### Task 8: Accessibility pass

**Files:**
- Create: `website/homepage/tests/e2e/a11y.spec.ts`
- Modify: `website/homepage/src/layouts/Base.astro`
- Modify: `website/homepage/src/components/*.astro`
- Modify: `website/homepage/src/styles/*.css`

**Interfaces:**
- Consumes: the complete page from Task 7.
- Produces: an automated axe check at mobile and desktop widths.

- [ ] **Step 1: Write the failing accessibility test**

`tests/e2e/a11y.spec.ts`:

```ts
import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

for (const width of [375, 1440]) {
  test(`no serious accessibility violations at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/');
    const results = await new AxeBuilder({ page }).analyze();
    const serious = results.violations.filter(
      (v) => v.impact === 'serious' || v.impact === 'critical',
    );
    expect(serious).toEqual([]);
  });
}

test('skip link reaches the main landmark', async ({ page }) => {
  await page.goto('/');
  await page.keyboard.press('Tab');
  await expect(page.locator('a.skip-link')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#main$/);
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `npm run test:e2e -- --grep "accessibility"`
Expected: FAIL on at least one axe violation or skip-link behavior.

- [ ] **Step 3: Fix the findings**

Typical fixes, applied only where the report points:

- Ensure one `h1` and no skipped heading levels.
- Raise muted text contrast to at least 4.5:1 on dark surfaces.
- Give the hero image meaningful alt text and decorative images empty alt.
- Add `aria-label` to the mobile summary and any icon-only link.
- Ensure every `<details><summary>` is reachable and has a visible focus ring.
- Ensure the skip link moves focus into `#main`; add `tabindex="-1"` to
  `<main>` if the browser does not move focus.
- Check external links that open a new tab; if any do, add
  `rel="noopener noreferrer"` and an accessible indication.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm run test:e2e`
Expected: PASS, all interaction and accessibility tests.

- [ ] **Step 5: Commit**

```bash
git add website/homepage
git commit -m "fix(website): resolve accessibility findings"
```

---

### Task 9: SEO, social image, sitemap, and 404

**Files:**
- Modify: `website/homepage/astro.config.mjs`
- Modify: `website/homepage/src/layouts/Base.astro`
- Create: `website/homepage/public/robots.txt`
- Modify: `website/homepage/public/favicon.svg`
- Create: `website/homepage/public/site.webmanifest`
- Create: `website/homepage/src/pages/404.astro`
- Create: `website/homepage/scripts/generate-og.mjs`
- Create: `website/homepage/public/og.png`
- Test: `website/homepage/tests/content/seo.test.mjs`
- Test: `website/homepage/tests/content/assets.test.mjs`

**Interfaces:**
- Consumes: `site` from Task 1.
- Produces: `og.png` at 1200×630, sitemap output, canonical URLs, and JSON-LD.

- [ ] **Step 1: Write the failing SEO/assets tests**

`tests/content/seo.test.mjs`:

```js
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as cheerio from 'cheerio';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const $ = cheerio.load(fs.readFileSync(path.join(root, 'dist/index.html'), 'utf8'));

test('SEO metadata and JSON-LD', () => {
  assert.equal($('link[rel="canonical"]').attr('href'), 'https://microwave.io/');
  assert.equal($('meta[property="og:image"]').attr('content'), 'https://microwave.io/og.png');
  assert.equal($('meta[name="twitter:card"]').attr('content'), 'summary_large_image');
  const json = JSON.parse($('script[type="application/ld+json"]').text());
  assert.equal(json['@type'], 'SoftwareApplication');
  assert.equal(json.name, 'Microwave Server');
  assert.equal(json.license, 'https://github.com/rmoriz/microwave-development/blob/main/LICENSE');
});

test('sitemap and robots exist', () => {
  assert.equal(fs.existsSync(path.join(root, 'dist/sitemap-index.xml')), true);
  assert.match(fs.readFileSync(path.join(root, 'dist/robots.txt'), 'utf8'), /Sitemap: https:\/\/microwave\.io/);
});
```

`tests/content/assets.test.mjs`:

```js
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

test('OG image is 1200x630', async () => {
  const meta = await sharp(path.join(root, 'public/og.png')).metadata();
  assert.equal(meta.width, 1200);
  assert.equal(meta.height, 630);
});

test('largest hero variant is under 300KB', () => {
  const files = fs.readdirSync(path.join(root, 'dist/_astro'));
  const heroes = files.filter((f) => /microwave-hero.*\.(avif|webp)$/.test(f));
  assert.ok(heroes.length > 0);
  for (const file of heroes) {
    const size = fs.statSync(path.join(root, 'dist/_astro', file)).size;
    assert.ok(size < 300 * 1024, `${file} is ${size} bytes`);
  }
});
```

- [ ] **Step 2: Run them to verify they fail**

Run: `node --test tests/content/seo.test.mjs tests/content/assets.test.mjs`
Expected: FAIL because metadata, sitemap, and OG image are missing.

- [ ] **Step 3: Implement SEO and assets**

- Add `sitemap()` to `astro.config.mjs` (already added in Task 1; verify it
  produces `sitemap-index.xml`).
- Add canonical, Open Graph, Twitter, and `SoftwareApplication` JSON-LD to
  `Base.astro`. Use `new URL(Astro.url.pathname, Astro.site)` for absolute URLs.
- `robots.txt`:

```text
User-agent: *
Allow: /

Sitemap: https://microwave.io/sitemap-index.xml
```

- `favicon.svg`: a rounded-square microwave-window glyph with the rainbow
  gradient and a solid fallback.
- `site.webmanifest`: name, short name, theme color `#0a0a0b`, icon
  `/favicon.svg`.
- `404.astro`: “This page went cold.” plus a link home. Use `noindex`.
- `scripts/generate-og.mjs`: use `sharp` to crop the hero to 1200×630, darken
  it, and composite an SVG overlay with “Get cooking without becoming a chef.”
  and “microwave.io”. Write `public/og.png`. Run `npm run og` and commit the
  result.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm run test:content`
Expected: PASS, including the SEO and asset tests.

- [ ] **Step 5: Commit**

```bash
git add website/homepage
git commit -m "feat(website): add SEO, social image, and 404"
```

---

### Task 10: Verify the Cinc Client claim end to end

**Files:**
- Modify: `website/homepage/src/components/CincSection.astro`
- Modify: `website/homepage/src/components/ProofStrip.astro`
- Modify: `website/homepage/tests/content/site.test.mjs`

**Interfaces:**
- Consumes: the conservative Cinc copy from Task 3.
- Produces: either the confirmed “tested” wording or the conservative wording,
  with tests matching the chosen copy.

- [ ] **Step 1: Run the Compose stack**

Run:

```bash
cd docker-e2e
docker compose up -d --build
docker compose wait seed
docker compose logs seed | tail -40
```

Wait for the seed to finish. Then verify each node:

```bash
docker compose exec -T node-debian systemctl is-active nginx
docker compose exec -T node-ubuntu systemctl is-active nginx
docker compose exec -T node-fedora systemctl is-active nginx
docker compose exec -T node-debian journalctl -u cinc-e2e-converge --no-pager | tail -20
```

Expected: each command prints `active`, and the journal shows a completed Cinc
run with no failures.

- [ ] **Step 2: Update the copy**

If all three nodes pass, change the Cinc section to “Tested with Cinc Client 19
on Debian, Ubuntu, and Fedora” and the proof cell to “3 distros tested with
Cinc Client 19”. Update the test to assert `tested with Cinc Client 19`.

If the stack does not pass, keep “ships an end-to-end Compose stack covering…”
and add a one-line note to the implementation report explaining the failure.
Do not claim “tested”.

- [ ] **Step 3: Tear down**

```bash
cd docker-e2e
docker compose down -v
```

- [ ] **Step 4: Run the content tests**

Run: `npm run test:content`
Expected: PASS with the chosen wording.

- [ ] **Step 5: Commit**

```bash
git add website/homepage
git commit -m "docs(website): confirm Cinc Client compatibility wording"
```

---

### Task 11: Deployment README and final verification

**Files:**
- Create: `website/homepage/README.md`
- Modify: `website/homepage/package.json` if scripts need adjustment

**Interfaces:**
- Consumes: everything above.
- Produces: a documented build/preview/deploy path and a final verification record.

- [ ] **Step 1: Write the website README**

Cover:

- What this directory is and the canonical domain.
- Requirements: Node 22+, npm.
- Commands: `npm install`, `npm run dev`, `npm run build`, `npm run preview`,
  `npm run check`, `npm run test:content`, `npm run test:e2e`, `npm run og`.
- Output: `dist/`; deploy to any static host.
- Example Cloudflare Pages settings: build command `npm run build`, output
  directory `dist`, root `website/homepage`.
- Example Netlify settings: same build command and publish directory.
- Legal launch checklist: supply `site.legal.operator` and `site.legal.address`,
  confirm `hello@microwave.io` works, confirm the GitHub URL, then rebuild.
- Copy maintenance: the Cinc claim is gated by Task 10; changing it requires
  rerunning the Compose stack.
- No analytics, cookies, or third-party requests.

- [ ] **Step 2: Run the full verification**

```bash
cd website/homepage
npm run check
npm run test
npm run build
```

Expected: type check, content tests, Playwright tests, and production build all
pass.

- [ ] **Step 3: Check the production page weight**

```bash
du -sh dist
find dist/_astro -type f -size +300k -print
```

Expected: no file over 300KB; `dist` is comfortably under 1MB for the initial
page payload.

- [ ] **Step 4: Run Lighthouse**

Start the preview server:

```bash
npm run preview -- --host 127.0.0.1 --port 4321
```

In another terminal:

```bash
npx lighthouse http://127.0.0.1:4321 \
  --chrome-path="/Applications/Chromium.app/Contents/MacOS/Chromium" \
  --only-categories=performance,accessibility,best-practices,seo \
  --output=json \
  --output-path=/tmp/microwave-lighthouse.json \
  --chrome-flags="--headless"
node -e "const r=require('/tmp/microwave-lighthouse.json'); for (const [k,v] of Object.entries(r.categories)) console.log(k, Math.round(v.score*100));"
```

Expected: all four categories at 95 or above. If not, fix the failing audit and
rerun.

- [ ] **Step 5: Review the built page manually**

- Open `http://127.0.0.1:4321` at 375px and 1440px.
- Confirm the hero image is sharp, the headline fits, and no text is clipped.
- Tab through the nav, CTA, accordions, FAQ, and footer.
- Confirm the mailto link opens with the subject prefilled.
- Confirm the legal pages show `[to be supplied]` until the owner supplies the
  details.
- Confirm no browser console errors and no network requests to third-party
  hosts.

- [ ] **Step 6: Commit**

```bash
git add website/homepage
git commit -m "docs(website): add deployment and verification guide"
```

---

## Self-Review Notes

- **Spec coverage:** hero, proof strip, Cinc section, compatibility, six
  technical panels, known gaps, migration, support, FAQ, final CTA, footer,
  privacy/imprint, responsive behavior, accessibility, SEO, OG image, 404, and
  deployment are each owned by a task.
- **Deliberate omissions:** no install command (owner choice), no live
  deployment (owner choice), no prices/SLAs (not supplied), no new
  compatibility report (link the repository's existing reports).
- **Launch blockers:** operator legal details, confirmed GitHub URL, working
  `hello@microwave.io` mailbox, and the Cinc wording gate in Task 10.
