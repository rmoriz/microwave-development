# Microwave homepage

The static landing page for [microwave.io](https://microwave.io), built with
Astro and vanilla CSS.

It is a single marketing page with a technical deep dive, plus privacy,
imprint, and 404 pages. There is no application server, no database, no
analytics, and no third-party runtime request.

## Requirements

- Node.js 22 or newer
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

- `npm run dev` starts the local development server.
- `npm run build` writes the static site to `dist/`.
- `npm run preview` serves `dist/` locally. Astro 7 may keep the preview server
  in the background; stop it with `npx astro preview stop`.
- `npm run check` runs the Astro and TypeScript checks.
- `npm run test:content` builds the site and runs the Node content, link, SEO,
  and asset assertions.
- `npm run test:e2e` runs Playwright against the production build. Install the
  browser once with `npx playwright install chromium`.
- `npm test` runs the type check, content tests, and browser tests.
- `npm run og` regenerates `public/og.png` from the hero image.

## Deployment

The build output is a plain static `dist/` directory. Any static host works.

### Cloudflare Pages

- Root directory: `website/homepage`
- Build command: `npm run build`
- Build output directory: `dist`
- Node version: 22 or newer

### Netlify

- Base directory: `website/homepage`
- Build command: `npm run build`
- Publish directory: `website/homepage/dist`
- Node version: 22 or newer

### Cache headers

- `/_astro/*`: `public, max-age=31536000, immutable`
- HTML: short-lived, revalidated on deploy

The site uses `https://microwave.io` as its canonical origin. Configure the
apex domain in the host and redirect `www` to the apex if both are enabled.

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
- Check the privacy page once the hosting provider is final. It intentionally
  does not name a provider or retention period yet.

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

## Structure

```text
src/
  assets/       hero image
  components/   one component per page section, plus nav and footer
  data/         site constants, links, and legal placeholders
  layouts/      Base layout, fonts, metadata, reveal script
  pages/        index, privacy, imprint, 404
  styles/       tokens, global rules, component styles
tests/
  content/      built-HTML assertions and link integrity
  e2e/          Playwright interaction and axe checks
scripts/
  generate-og.mjs
public/
  favicon.svg, og.png, robots.txt, site.webmanifest
```
