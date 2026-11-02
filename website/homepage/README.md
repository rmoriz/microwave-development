# Microwave homepage

The landing page for the project published at
[https://rmoriz.github.io/microwave-development/](https://rmoriz.github.io/microwave-development/),
built with Astro and vanilla CSS.

It is a single marketing page with a technical deep dive, plus privacy,
imprint, and 404 pages. There is no application server, no database, no
analytics, and no third-party runtime request. The MCP demo player
(`asciinema-player`) is the one front-end dependency: it is self-hosted and
loaded only when someone presses play.

## Requirements

- Node.js 22.18 or newer (unflagged TypeScript type stripping)
- npm

## Commands

```sh
npm install
npm run dev
npm run build
npm run preview
npm run check
npm run test:content
npm run test:e2e
npm test
```

- `npm run dev` serves at `http://localhost:4321/microwave-development/`.
- `npm run build` writes the static site to `dist/`.
- `npm run preview` serves `dist/` locally. Astro 7 may keep the preview server
  in the background; stop it with `npx astro preview stop`.
- `npm run check` runs the Astro and TypeScript checks.
- `npm run test:content` builds the site and runs the Node content, link, SEO,
  and asset assertions.
- `npm run test:e2e` runs Playwright against the production build, served from
  `.preview/microwave-development/` so the base path is exercised. Install the
  browser once with `npx playwright install chromium`.
- `npm test` runs the type check, content tests, and browser tests.
- `npm run og` regenerates `public/og.png` from the hero image.

## Deployment

The build output is a plain static `dist/` directory. Astro writes every
internal URL under the configured base path, so `dist/` must be served from
`/microwave-development/` (the base in `src/data/config.ts`), not from a domain
root.

### GitHub Pages

`.github/workflows/pages.yml` builds the homepage and the docs into one artifact
and deploys it to `https://rmoriz.github.io/microwave-development/` on every
push to `main` that touches the sites or a published guide, and on manual
dispatch. A manual dispatch must be started on `main`: the `deploy` job is
gated to `refs/heads/main`, because a branch run has no `github-pages`
environment deployment.

- Internal URLs must go through `withBase()` from `src/lib/paths.ts`.
- `SITE_URL` and `BASE_PATH` live in `src/data/config.ts`; `robots.txt` and the
  sitemap are generated from them.
- `BASE_PATH` and the docs `baseUrl` must move together.

### Headers and policy

Pages accepts no header configuration and fixes its own: responses carry
`cache-control: max-age=600` and no `X-Frame-Options`. What the site can
actually set is therefore its security policy, as `<meta http-equiv>` tags in
`src/layouts/Base.astro`:

- `Content-Security-Policy`: `default-src 'self'`; scripts from `'self'`,
  `'unsafe-inline'` and `https://www.youtube.com` (the iframe API), the
  asciinema player's WebAssembly needs `'wasm-unsafe-eval'` instead of
  `'unsafe-eval'`; frames only from `https://www.youtube-nocookie.com`; images
  from `'self'` and `data:`; fonts from `'self'` and `data:` (Astro inlines the
  small subsets)
- `X-Content-Type-Options: nosniff`
- `X-XSS-Protection: 1`
- `Referrer-Policy: no-referrer`

`X-Frame-Options` cannot be expressed in a meta tag, so `frame-ancestors 'none'`
in the CSP carries that job. Browsers currently only honour `frame-ancestors`
from a real header, so the meta tag documents the intent and a header layer
would enforce it. The e2e console check ignores the browser's one-line notice
for that directive, nothing else.

A header layer would add real cache headers on top of the fixed
`max-age=600`, so these values are the intent, not what Pages serves today:

- `${BASE_PATH}/_astro/*`: `public, max-age=31536000, immutable` — the file
  names are content-hashed, so a year and `immutable` are safe
- HTML: short-lived, revalidated on deploy

## Before launch

- Fill in `site.legal.operator` and `site.legal.address` in
  `src/data/site.ts`. The imprint renders `[to be supplied]` and a visible
  warning until those values exist. Launch is blocked until they are real.
- Confirm `hello@microwave.io` receives mail and update `site.legal.email` if
  the support address differs.
- Confirm the GitHub URL in `src/data/site.ts` is the public repository.
- Confirm the Cinc claim. It currently says “Tested with Cinc Client 19 on
  Debian, Ubuntu, and Fedora.” That was verified by running
  `docker-e2e/docker-compose.yml` and checking all three nodes. Re-run that
  stack before changing the claim.
- Check the privacy page once hosting is final. It names GitHub Pages as the
  host and links GitHub's privacy statement, but names no retention period.

## Content rules

- No pricing attacks or competitor disparagement.
- No prices, tiers, SLAs, or response times for commercial support.
- No invented customers, testimonials, logos, benchmarks, or certifications.
- Chef and Cinc are referenced only to describe compatibility. Do not use their
  logos or imply endorsement.
- The footer independence and trademark disclaimer is mandatory.
- Keep factual claims traceable to the repository. The content tests enforce
  the most important ones, including the Cinc wording and the absence of
  forbidden competitor language.
- MCP is always described as experimental. Reads come before writes; writes are
  on by default and switched off with `writes = false`. Keep the wording
  traceable to `docs/mcp.md` and do not use “AI-powered” or “built-in AI”. The
  content test enforces this.
- `public/demo/mcp-agent.cast` is the MCP demo. It must stay anonymized
  (`workstation.local`, `user`, `/home/user/`), stay under 400 KB, and the
  caption must match what the recording actually shows.

## Structure

```text
src/
  assets/       hero image
  components/   one component per page section, plus nav and footer
  data/         site constants, links, legal placeholders, canonical origin
  layouts/      Base layout, fonts, metadata, reveal script
  lib/          base-path helper
  pages/        index, privacy, imprint, 404, robots.txt route
  styles/       tokens, global rules, component styles
tests/
  content/      built-HTML assertions and link integrity
  e2e/          Playwright interaction and axe checks
scripts/
  generate-og.mjs
public/
  favicon.svg, og.png, site.webmanifest, demo/mcp-agent.cast
```

<!-- gate check -->
