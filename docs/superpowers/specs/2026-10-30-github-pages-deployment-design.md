# GitHub Pages for Homepage and Docs — Design

## Intent

Publish the homepage and the curated documentation together on GitHub Pages so
the project has a working public site before `microwave.io` is owned. The site
lives under the repository's Pages URL, `https://rmoriz.github.io/microwave-development/`,
with the docs at `/docs/`. The operator picked this path over waiting for the
domain.

## Goals

- One Pages deployment containing the homepage at `/microwave-development/`
  and the docs at `/microwave-development/docs/`.
- Every internal URL, asset, canonical, sitemap, manifest, and test works under
  that base path.
- Deploys only from `main` (and manual dispatch), using the official Pages
  actions and a protected `github-pages` environment.
- Switching to a custom domain later means changing one base-path constant and
  the DNS, not rewriting links again.
- Docs links on the homepage point at the published docs site instead of
  GitHub blobs where the curated page exists.

## Non-goals

- No custom domain now. `microwave.io` is registered but parked at Atom and not
  under project control.
- No second repository, no second Pages site.
- No PR preview deployments.
- No analytics, framework, or runtime request on the homepage.
- No changes to Rust, CI path filters, or release automation.

## Anti-goals

- Root-absolute links that silently leave the base path.
- A deploy that runs before the built HTML is checked.
- Leaving the old Cloudflare/Netlify instructions as the only documented path.
- A workflow that deploys on every branch or needs repository secrets.

## Constraints

- Public repository, so Pages is free; Pages is not enabled yet.
- GitHub Pages allows one site per repository and no cache-header or redirect
  configuration. `_astro/` needs `.nojekyll`.
- Homepage rules in `website/homepage/README.md` and the content tests still
  apply.
- `website/docs` builds with `onBrokenLinks: 'throw'`.
- No fixed sleeps in CI; check the run with `gh run watch <id> --exit-status`.

## Approaches

- **A — one combined artifact under the repository Pages path (chosen).** Fits
  the free public-repo plan, needs no domain, and keeps the homepage canonical
  origin honest.
- **B — buy `microwave.io` first, then deploy at the apex.** Almost no source
  changes and correct canonical URLs, but blocked on the sale and DNS.
- **C — split homepage and docs into two repositories.** Two Pages sites, but
  two copies of the docs source or a publishing pipeline between them.

## Decisions

- `src/data/config.ts` is the single source for `SITE_URL`
  (`https://rmoriz.github.io`) and `BASE_PATH` (`/microwave-development`).
  `astro.config.mjs`, the canonical/OG code, the tests, the Playwright config,
  and `robots.txt` read it.
- New `src/lib/paths.ts` exposes `withBase()`; every internal `href`/`src` goes
  through it. When `BASE_PATH` becomes `''`, the helper is a no-op.
- `Astro.site` plus the already-based `Astro.url.pathname` produce the
  canonical; the social image goes through `withBase()` before it is resolved.
- `public/robots.txt` becomes `src/pages/robots.txt.ts`, generated from the
  same constants. `public/site.webmanifest` uses relative `start_url` and icon
  URLs so it works on any base.
- `website/docs/docusaurus.config.ts` gets the new `url` and
  `baseUrl: '/microwave-development/docs/'`; navbar and footer home links point
  at the Pages homepage, and the Docs links on the homepage point at
  `/docs/` and `/docs/mcp`.
- Content tests strip `BASE_PATH` before resolving internal targets and assert
  canonical, OG image, and sitemap against `${SITE_URL}${BASE_PATH}`.
- Playwright serves the build under a temporary `microwave-development/`
  parent directory, sets `baseURL` to the origin, and navigates with
  `${BASE_PATH}/…`.
- New `.github/workflows/pages.yml`: on `main` pushes touching the sites or
  `docs/`, and on `workflow_dispatch`. Build job runs `npm run check`,
  `npm run test:content`, docs `typecheck` and `build`, copies the docs build
  to `dist/docs`, writes `.nojekyll`, and uploads the artifact. Deploy job uses
  `actions/deploy-pages`.
- Pages is enabled via the API with `build_type=workflow` before the first
  dispatch.

## Risks

- GitHub Pages serves its own cache headers; the README cache section is
  rewritten rather than promised.
- A missing `.nojekyll` drops `_astro/`. It is written in the same step that
  assembles the artifact.
- The docs 404 falls back to the homepage 404. Accepted.
- If Pages is unavailable, the artifact still builds locally and the workflow
  fails visibly.

## Tests

- `cd website/homepage && npm test` (type check, content, Playwright with axe)
  green under the base path.
- `cd website/docs && npm run typecheck && npm run build` green.
- `actionlint` green for the new workflow.
- After dispatch: `gh run watch <id> --exit-status`, then fetch
  `/microwave-development/`, `/microwave-development/docs/mcp/`, the favicon,
  a CSS asset, and `robots.txt` and check status and base-prefixed URLs.
