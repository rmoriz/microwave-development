# GitHub Pages Deployment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish the homepage and curated docs on GitHub Pages under `https://rmoriz.github.io/microwave-development/`.

**Architecture:** Both static builds are assembled into one Pages artifact: the homepage at the root, the docs under `docs/`. A single `SITE_URL`/`BASE_PATH` module feeds Astro, the canonical/OG code, `robots.txt`, and the tests, so a later custom domain is a constant change. A `pages.yml` workflow builds, checks, assembles, and deploys via the official Pages actions.

**Tech Stack:** Astro 7, Docusaurus 3.10, Node 24, Playwright, GitHub Actions Pages.

**Spec:** `docs/superpowers/specs/2026-10-30-github-pages-deployment-design.md`

## Global Constraints

- `SITE_URL` is `https://rmoriz.github.io`; `BASE_PATH` is `/microwave-development`; docs base is `/microwave-development/docs/`.
- Internal URLs always go through `withBase()`; `BASE_PATH = ''` must make it a no-op.
- No new dependency, no client-side framework, no runtime request.
- English copy; homepage content rules in `website/homepage/README.md` stay binding.
- Node 24 in CI; homepage requires Node 22 or newer.
- Pages is one site per repository, deployed only from `main` or manual dispatch.
- `.nojekyll` and a docs `index.html` must be present in the artifact before upload.

## Review Focus

- A new root-absolute link skips the base path and leaves the site → content link test asserts every internal `href` starts with `BASE_PATH`.
- Canonical, OG image, or sitemap point at the origin root → SEO test asserts all three against `${SITE_URL}${BASE_PATH}`.
- Jekyll drops `_astro/` because `.nojekyll` is missing → workflow asserts the file, live check fetches a hashed CSS asset.
- The docs land at the artifact root instead of `docs/` → workflow asserts `dist/docs/index.html`, live check fetches the MCP page.
- The workflow deploys from a pull request or lacks Pages permissions → trigger block and job permissions are fixed; `actionlint` plus a manual run verify it.

---

### Task 1: Homepage Base Path and Internal URLs

**Files:**
- Create: `website/homepage/src/data/config.ts`
- Create: `website/homepage/src/lib/paths.ts`
- Create: `website/homepage/src/pages/robots.txt.ts`
- Create: `website/homepage/tests/e2e/base.ts`
- Modify: `website/homepage/astro.config.mjs`
- Modify: `website/homepage/src/data/site.ts`
- Modify: `website/homepage/src/layouts/Base.astro:25-56`
- Modify: `website/homepage/src/components/Logo.astro:1-4`
- Modify: `website/homepage/src/components/Nav.astro:1-12`
- Modify: `website/homepage/src/components/Footer.astro:1-24`
- Modify: `website/homepage/src/pages/404.astro:19`, `privacy.astro:32`, `imprint.astro:28`
- Modify: `website/homepage/public/site.webmanifest`
- Delete: `website/homepage/public/robots.txt`
- Modify: `website/homepage/playwright.config.ts`
- Modify: `website/homepage/.gitignore`
- Modify: `website/homepage/tests/content/site.test.mjs`
- Modify: `website/homepage/tests/content/seo.test.mjs`
- Modify: `website/homepage/tests/content/links.test.mjs`
- Modify: `website/homepage/tests/e2e/home.spec.ts`, `a11y.spec.ts`, `palette.spec.ts`, `grid.spec.ts`

**Interfaces:**
- Produces: `SITE_URL: string`, `BASE_PATH: string` from `src/data/config.ts`; `withBase(path: string): string` from `src/lib/paths.ts`; `url(path = '/'): string` from `tests/e2e/base.ts`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing SEO and link assertions**

In `tests/content/seo.test.mjs`, replace the first two assertions with constants from the config:

```js
import { BASE_PATH, SITE_URL } from '../../src/data/config.ts';

const home = `${SITE_URL}${BASE_PATH}/`;
// ...
  assert.equal($('link[rel="canonical"]').attr('href'), home);
  assert.equal($('meta[property="og:image"]').attr('content'), `${home}og.png`);
  assert.equal($('meta[name="twitter:image"]').attr('content'), `${home}og.png`);
```

Change the robots assertion to:

```js
  assert.match(
    fs.readFileSync(path.join(root, 'dist/robots.txt'), 'utf8'),
    new RegExp(`Sitemap: ${SITE_URL}${BASE_PATH}/sitemap-index.xml`),
  );
```

In `tests/content/links.test.mjs`, import the constant, add the base assertion, and strip the base before resolving:

```js
import { BASE_PATH } from '../../src/data/config.ts';

function withoutBase(href) {
  if (href === BASE_PATH) return '/';
  return href.startsWith(`${BASE_PATH}/`) ? href.slice(BASE_PATH.length) : href;
}
```

Inside the `$('a[href]').each` callback, after the protocol checks:

```js
      assert.ok(
        href === BASE_PATH || href.startsWith(`${BASE_PATH}/`),
        `internal href outside ${BASE_PATH} at ${where}`,
      );

      const [rawPath, hash] = withoutBase(href).split('#');
```

- [ ] **Step 2: Run the assertions to verify they fail**

Run: `cd website/homepage && npm run build && node --test tests/content/seo.test.mjs tests/content/links.test.mjs`
Expected: FAIL — canonical is `https://rmoriz.github.io/`, and internal hrefs start with `/`, not `/microwave-development`.

- [ ] **Step 3: Add the shared config and path helper**

`src/data/config.ts`:

```ts
export const SITE_URL = 'https://rmoriz.github.io';
export const BASE_PATH = '/microwave-development';
```

`src/lib/paths.ts`:

```ts
import { BASE_PATH } from '../data/config';

const base = BASE_PATH.replace(/\/$/, '');

export function withBase(path: string): string {
  return `${base}${path.startsWith('/') ? path : `/${path}`}`;
}
```

- [ ] **Step 4: Point Astro and the metadata at the base path**

In `astro.config.mjs`:

```js
import { BASE_PATH, SITE_URL } from './src/data/config.ts';

export default defineConfig({
  site: SITE_URL,
  base: BASE_PATH,
  // integrations and build stay unchanged
});
```

In `src/data/site.ts`, import `SITE_URL` and `withBase`, set `url: SITE_URL` and `docs: withBase('/docs/')`.

In `src/layouts/Base.astro`, import `withBase` and `SITE_URL`; change `const origin = Astro.site ?? new URL(SITE_URL)` and:

```astro
  image = withBase('/og.png'),
  // ...
  const socialImage = new URL(withBase(image), origin);
```

Replace the icon and manifest tags with:

```astro
    <link rel="icon" type="image/svg+xml" href={withBase('/favicon.svg')} />
    <link rel="manifest" href={withBase('/site.webmanifest')} />
```

- [ ] **Step 5: Generate robots.txt and make the manifest relative**

Delete `public/robots.txt`. Create `src/pages/robots.txt.ts`:

```ts
import type { APIRoute } from 'astro';
import { BASE_PATH, SITE_URL } from '../data/config';

export const GET: APIRoute = () => {
  const sitemap = new URL(`${BASE_PATH}/sitemap-index.xml`, SITE_URL);
  return new Response(
    `User-agent: *\nAllow: /\n\nSitemap: ${sitemap.href}\n`,
    { headers: { 'Content-Type': 'text/plain; charset=utf-8' } },
  );
};
```

In `public/site.webmanifest`, set `"start_url": "./"` and the icon `"src": "favicon.svg"`.

- [ ] **Step 6: Prefix every internal component and page link**

`Logo.astro`: import `withBase`; `href={withBase('/')}`.

`Nav.astro`: import `withBase`; wrap all six `href` values, e.g. `{ href: withBase('/#product'), label: 'Product' }`.

`Footer.astro`: import `withBase`; apply to `/privacy/`, `/imprint/`, `/#faq`; `site.docs` already carries the base.

`404.astro`, `privacy.astro`, `imprint.astro`: import `withBase`; the back link becomes `href={withBase('/')}`.

- [ ] **Step 7: Serve the build under the base path in Playwright and update the specs**

`tests/e2e/base.ts`:

```ts
import { BASE_PATH } from '../../src/data/config';

export const url = (path = '/') => `${BASE_PATH}${path}`;
```

`playwright.config.ts`: import `BASE_PATH`; set `baseURL: 'http://127.0.0.1:4321'`; replace the webServer entry:

```ts
  webServer: {
    command:
      'npm run build && rm -rf .preview && mkdir -p .preview/microwave-development && cp -R dist/. .preview/microwave-development/ && python3 -m http.server 4321 --bind 127.0.0.1 --directory .preview',
    url: `http://127.0.0.1:4321${BASE_PATH}/`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
```

Add `.preview/` to `.gitignore`.

In all four e2e specs, import `{ url } from './base'`. Replace every `page.goto('/')` with `page.goto(url())` and `page.goto('/privacy/')` with `page.goto(url('/privacy/'))`. In `home.spec.ts`, add `const product = url('/#product')` and `const agents = url('/#agents')`, then use `page.locator(\`.nav-mobile a[href="${product}"]\`)`, `page.locator(\`.nav-desktop a[href="${product}"]\`)`, and the same for `agents`. Change the expected legal-page URL regex to `/\/#product$/`.

- [ ] **Step 8: Update the content shell assertions**

In `tests/content/site.test.mjs`, import `{ BASE_PATH }` and change:

```js
  assert.equal($(`.nav-desktop a[href="${BASE_PATH}/#agents"]`).length, 1);
  assert.equal($(`.nav-mobile a[href="${BASE_PATH}/#agents"]`).length, 1);
  // ...
  assert.equal($(`a[href="${BASE_PATH}/privacy/"]`).length, 1);
  assert.equal($(`a[href="${BASE_PATH}/imprint/"]`).length, 1);
```

- [ ] **Step 9: Run the full homepage suite**

Run: `cd website/homepage && npm test`
Expected: PASS for `astro check`, all content tests, and all Playwright tests.

- [ ] **Step 10: Commit**

```bash
git add website/homepage
git commit -m "feat(website): serve the homepage under a configurable base path"
```

---

### Task 2: Docs Base Path and Cross-Links

**Files:**
- Modify: `website/docs/docusaurus.config.ts:10-11,82,113-116`
- Modify: `website/docs/README.md`

**Interfaces:**
- Consumes: `BASE_PATH` value `/microwave-development` from Task 1 (literal here; Docusaurus does not import the homepage module).
- Produces: docs routes under `/microwave-development/docs/`.

- [ ] **Step 1: Point the docs at the Pages path**

In `docusaurus.config.ts` set:

```ts
  url: 'https://rmoriz.github.io',
  baseUrl: '/microwave-development/docs/',
```

Change the navbar `href: 'https://microwave.io'` entry to `href: 'https://rmoriz.github.io/microwave-development/'` with `label: 'Homepage'`. Change the footer project link the same way and relabel it `Homepage`.

- [ ] **Step 2: Document the base path**

In `website/docs/README.md`, add after the commands block:

```md
The site deploys to GitHub Pages under
`https://rmoriz.github.io/microwave-development/docs/`. `baseUrl` must stay in
sync with the homepage's `BASE_PATH`.
```

- [ ] **Step 3: Verify the docs build**

Run: `cd website/docs && npm run typecheck && npm run build`
Expected: build succeeds; output contains `build/index.html` and asset URLs starting with `/microwave-development/docs/`.

Run: `grep -o 'href="/microwave-development/docs/' website/docs/build/index.html | head -1`
Expected: one match.

- [ ] **Step 4: Commit**

```bash
git add website/docs
git commit -m "feat(docs): publish under the Pages base path"
```

---

### Task 3: Pages Workflow and Documentation

**Files:**
- Create: `.github/workflows/pages.yml`
- Modify: `website/homepage/README.md:39-56`
- Modify: `AGENTS.md` (Website rules)

**Interfaces:**
- Consumes: `SITE_URL`/`BASE_PATH` from Task 1 and the docs base from Task 2.
- Produces: a `pages` workflow with `build` and `deploy` jobs.

- [ ] **Step 1: Add the workflow**

```yaml
name: pages

on:
  push:
    branches: [main]
    paths:
      - 'website/homepage/**'
      - 'website/docs/**'
      - 'docs/**'
      - '.github/workflows/pages.yml'
  workflow_dispatch:

permissions:
  contents: read

concurrency:
  group: pages
  cancel-in-progress: false

jobs:
  build:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: "24"
          cache: npm
          cache-dependency-path: |
            website/homepage/package-lock.json
            website/docs/package-lock.json
      - name: Build the homepage
        working-directory: website/homepage
        run: |
          npm ci
          npm run check
          npm run test:content
      - name: Build the docs
        working-directory: website/docs
        run: |
          npm ci
          npm run typecheck
          npm run build
      - name: Assemble the site
        run: |
          set -euo pipefail
          mkdir -p website/homepage/dist/docs
          cp -R website/docs/build/. website/homepage/dist/docs/
          touch website/homepage/dist/.nojekyll
          test -f website/homepage/dist/.nojekyll
          test -f website/homepage/dist/docs/index.html
      - uses: actions/configure-pages@v5
      - uses: actions/upload-pages-artifact@v3
        with:
          path: website/homepage/dist

  deploy:
    needs: build
    runs-on: ubuntu-24.04
    permissions:
      pages: write
      id-token: write
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - id: deployment
        uses: actions/deploy-pages@v4
```

- [ ] **Step 2: Replace the deployment section**

In `website/homepage/README.md`, replace the Cloudflare Pages and Netlify sections with:

```md
### GitHub Pages

`.github/workflows/pages.yml` builds the homepage and the docs into one artifact
and deploys it to `https://rmoriz.github.io/microwave-development/` on every
push to `main` that touches the sites or `docs/`, and on manual dispatch.

- Internal URLs must go through `withBase()` from `src/lib/paths.ts`.
- `SITE_URL` and `BASE_PATH` live in `src/data/config.ts`; `robots.txt` and the
  sitemap are generated from them.
- `BASE_PATH` and the docs `baseUrl` must move together.
- Pages has no cache-header configuration; it sets its own.
```

Leave the other host notes only if they stay true; otherwise delete them.

- [ ] **Step 3: Add the website rule to AGENTS.md**

Under “Website rules”, add:

```md
- Internal URLs go through `withBase()`; `BASE_PATH` and the docs `baseUrl`
  change together. Pages serves the site under `/microwave-development/`.
```

- [ ] **Step 4: Lint the workflow**

Run:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/rhysd/actionlint/main/scripts/download-actionlint.bash) 1.7.12
./actionlint .github/workflows/pages.yml
```

Expected: no output, exit 0.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/pages.yml website/homepage/README.md AGENTS.md
git commit -m "feat(website): deploy homepage and docs to GitHub Pages"
```

---

### Task 4: Enable Pages and Verify the Live Site

**Files:** none.

**Interfaces:**
- Consumes: the workflow from Task 3 and the artifacts from Tasks 1–2.
- Produces: a live pages deployment.

- [ ] **Step 1: Enable Pages for Actions**

Run:

```bash
gh api -X POST repos/rmoriz/microwave-development/pages \
  -H 'Accept: application/vnd.github+json' -f build_type=workflow
```

Expected: `build_type` is `workflow`. On 409, read it with `gh api repos/rmoriz/microwave-development/pages` and continue.

- [ ] **Step 2: Dispatch the workflow on the feature branch**

Run: `gh workflow run pages.yml --ref feat/docs-site`
Then: `gh run list --workflow pages.yml --limit 1` and `gh run watch <id> --exit-status`.
Expected: the run succeeds.

- [ ] **Step 3: Check the live output**

Run each and expect the stated result:

```bash
curl -sS -o /dev/null -w '%{http_code}\n' https://rmoriz.github.io/microwave-development/          # 200
curl -sS -o /dev/null -w '%{http_code}\n' https://rmoriz.github.io/microwave-development/docs/mcp/ # 200
curl -sS https://rmoriz.github.io/microwave-development/robots.txt                                 # Sitemap contains /microwave-development/
curl -sS -o /dev/null -w '%{http_code}\n' https://rmoriz.github.io/microwave-development/sitemap-index.xml # 200
```

Fetch the homepage, extract a `/_astro/` URL, and confirm it returns 200. Confirm the canonical contains `/microwave-development/`.

- [ ] **Step 4: Report the URL and remaining follow-up**

Report `https://rmoriz.github.io/microwave-development/` and note that the custom-domain switch is a later change to `SITE_URL`, `BASE_PATH`, and the docs `baseUrl`, plus DNS.

## Self-Review

- Spec coverage: one artifact (Tasks 1–3), base-safe links/canonical/robots/manifest (Task 1), docs base (Task 2), workflow and `.nojekyll` (Task 3), Pages enablement and live verification (Task 4), custom-domain path documented (Task 4). No PR previews and no Rust changes, as specified.
- Names and signatures agree: `withBase`, `url`, `SITE_URL`, `BASE_PATH` are used exactly as defined.
- Review Focus lines each have an owning assertion or check.
- No new dependency and no content-copy change beyond the documented host link.
