# Landing page for browsers, with security headers and a shared favicon

## Intent

A request to `/` on a real Chef Infra Server is answered by nginx with a static
`index.html`: a friendly page for humans, because every API route expects
signing headers no browser sends. Chef clients never see it. Microwave has no
such page — `/` currently falls through to the JSON `404` behind the auth
middleware, so a browser gets `401`.

This change gives Microwave the same surface: a static page for requests that
carry no client-version header, built in the visual language of
`website/homepage`, carrying a link to that site. It ships with a CSP and the
hardening headers the user asked for, and the header logo mark is turned into
one favicon used by the homepage, the docs site, and the server page.

## Behavior

Upstream's precedence comes from nginx `location` blocks, not from a rule in
the application: some paths are answered by their own `location` (and their own
upstream) before `location /` can send a browser to `index.html`. Microwave
mirrors exactly that split.

**Always static, no version header needed** (upstream's allowlisted static
locations, outside `location /`):

- `GET`/`HEAD` `/index.html` → the page
- `GET`/`HEAD` `/favicon.svg` → the icon
- `GET`/`HEAD` `/css/<file>.css` → `tokens.css`, `global.css`,
  `components.css`, `fonts.css`, `landing.css`, exact names only
- `GET`/`HEAD` `/fonts/<file>.woff2` → the two font files, exact names only

**Served as the page when no client marks the request either way**, for every
method, on every path that is not one of the paths below — matched API routes
included. A browser hitting `/users` gets the page, exactly as it does
upstream.

"Either way" means: no `X-Chef-Version`, no `X-Cinc-Version`, and no complete
signature (`has_signing_headers`, so all four `X-Ops-*` headers plus an
authorization line). The signing guard is a deliberate step beyond upstream:
Chef Client always sends `X-Chef-Version`, Cinc Client a version header of its
own, so a signed request without either is a broken or hand-rolled client that
should see the API's `401`, not HTML — and it keeps the existing test helper
and its 112 call sites working unchanged.

**Exempt even without a version header or signature** (upstream answers these
from their own locations, before `location /`): `/version`, `/_status`,
`/_stats`, `/server_api_version`, `/id`, `/id/*`, `/bookshelf/*`, `…/validate`,
`…/required_recipe`, `…/data-collector`, `…/mcp`, and the compliance proxy
path. The existing pre-auth path block in `authenticate_request` already names
most of these; the predicate is shared, not duplicated.

**Version header present:** unchanged behavior. `X-Chef-Version` wins when both
are sent, then `X-Cinc-Version`; either is checked against the existing
minimum of 10 and produces the existing `400` message. `X-Cinc-Version` is new
code: today the middleware reads `x-chef-version` only and a headerless request
simply skips the gate.

Before the plan is executed, verify on `docker-e2e` that an actual Cinc Client
converge sends one of the two headers (`tcpdump` on the client container, or
the access log with the header filtered out of the signature). If Cinc Client
in the pinned version sends neither, the signing guard above still keeps those
converges on the API, and the plan records it.

The page is served before the maintenance-mode check, because upstream never
reaches erchef for it: during a drain a browser still gets the page while API
calls get their `503`.

## Goals

- `/` and unmatched paths answer `200 text/html` for browsers instead of
  `401`/`404` JSON.
- The page looks like the homepage, not like a plain-text placeholder.
- Every static response carries
  `Content-Security-Policy`, `X-Frame-Options: DENY`,
  `X-Content-Type-Options: nosniff` and `X-XSS-Protection: 1`.
- One favicon source, rendered from the logo mark, on homepage, docs and
  server.
- Homepage and docs carry the same policy as far as their host allows.

## Non-goals

- Upstream's `404.html` and `503.json` error pages. A browser reaching a
  version-header path still gets the JSON body.
- HSTS.
- A web UI, any JavaScript on the server page, or a second static file host.
- Real response headers on the published sites: GitHub Pages sets its own, so
  the sites get `<meta http-equiv>` tags.
- Translation, `apple-touch-icon`, or a raster `.ico`.
- Headers on API responses.

## Anti-goals

- Two divergent token sets. The server page must not carry its own copy of the
  colors or radii.
- Accepting `'unsafe-inline'` in the server page's CSP.
- A `cargo build` that needs Node, `npm ci`, or a generated asset checked into
  the tree.
- Splitting the browser rule across the router and the middleware in two
  places that can drift.

## Constraints

- `website/homepage/README.md` is binding for copy: English, playful cooking
  angle, no invented metrics, MCP always `experimental`, writes on by default
  and switched off with `writes = false`, never `AI-powered`.
- The page must work with JavaScript disabled, avoid horizontal overflow at
  320/375/768/900/1440 px, keep one `h1` and a sane heading order.
- The site URL is `https://rmoriz.github.io/microwave-development/`; the link
  is absolute and defined in one place, next to the page.
- Embedded sources are read with `include_str!`/`include_bytes!`. The three site
  stylesheets live in `website/homepage/src/styles/`; the two fonts are copied
  into `crates/microwave-api/assets/fonts/` because `node_modules` is not part
  of a checkout.
- The homepage's own CSP must keep its current inline module scripts and the
  YouTube player working; the docs CSP must keep Docusaurus hydration working.
- The page is static: no JavaScript, no images, and the mono text falls back to
  the token's system-mono stack rather than shipping a third font.

## Approaches considered

**A — own minimal page, tokens copied.** Isolated and small, but the colors
and type scale drift away from the site with the first redesign.

**B — own page, real site classes, site stylesheets embedded unchanged** (chosen).
`include_str!` pulls `tokens.css`, `global.css` and `components.css` straight
from `website/homepage/src/styles/`; the page uses `.site-header`, `.logo`,
`.hero`, `.button`. Full fidelity, no copy, no Node in the Rust build. Costs
~29 KB CSS and ~52 KB fonts in the binary, and a marketing refactor can reach
the server page.

**C — generate from Astro.** Real component reuse, but Node in the Rust build,
generated files in the repository, and a freshness test.

## Design

### Assets

`crates/microwave-api/assets/`:

- `landing/index.html` — the page
- `landing/landing.css` — the few page-specific rules, loaded after the site
  stylesheets
- `landing/fonts.css` — two `@font-face` rules, family names
  `'Inter Variable'` and `'Space Grotesk Variable'` so the tokens resolve
- `landing/favicon.svg` — the favicon, from the logo mark
- `fonts/inter-latin.woff2`, `fonts/space-grotesk-latin.woff2` — copied from
  `@fontsource-variable`, latin, variable weight
- `fonts/OFL.txt` — SIL Open Font License text, with the package versions

Stylesheet sources: `tokens.css`, `global.css` and `components.css` are
embedded with `include_str!` from `website/homepage/src/styles/`; `fonts.css`
and `landing.css` live in `assets/landing/`. `OFL.txt` is repository material
and is not served.

`crates/microwave-api/src/landing.rs` embeds them and exposes:

- `page() -> Response` — `text/html; charset=utf-8`, `Cache-Control: no-cache`,
  `Vary: X-Chef-Version, X-Cinc-Version`
- `stylesheet(name) -> Option<Response>` — one exact filename,
  `text/css; charset=utf-8`, `Cache-Control: public, max-age=3600`
- `favicon() -> Response` — `image/svg+xml`, same caching
- `font(name) -> Option<Response>` — `font/woff2`, same caching
- `apply_headers(&mut Response)` — the header block below

The `Vary` belongs on the page only: the same URL yields HTML or JSON depending
on a request header, so a proxy without it would hand the page to a Chef client
from cache. API responses are identical regardless of the header.

Static routes go on the outer router next to `/version`, so they bypass
signature authentication and API-version negotiation entirely. The browser
branch in `authenticate_request` (top of the function, after
`parse_api_version`, before the version check and before maintenance mode)
returns `landing::page()`.

### Headers

On every static response, and only there (`Vary` comes from `page()`):

```
Content-Security-Policy: default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'
X-Frame-Options: DENY
X-Content-Type-Options: nosniff
X-XSS-Protection: 1
Referrer-Policy: no-referrer
```

No script source at all. `X-XSS-Protection: 1` is legacy but requested and
harmless; `frame-ancestors` and `X-Frame-Options` are redundant on purpose,
because the meta variant on the sites cannot express `X-Frame-Options`.

`Referrer-Policy` and the `Cache-Control` values are my calls: they keep the
page out of the way of `/_status` URLs operators paste around, and keep a
server upgrade visible without a stale cache.

### Page content

One screen, no JavaScript, no images:

- `<head>`: `lang="en"`, title `Microwave Server`, a one-sentence description,
  `<link rel="icon" type="image/svg+xml" href="/favicon.svg">`, the five
  stylesheets, font preloads.
- Header: the mark as inline SVG plus the word mark, linking to the website;
  nav links `Docs`, `GitHub`.
- `<h1>` "This is a Microwave server." with the accent gradient on the second
  part, a sub-line stating what it is (independent, open-source Chef Infra
  Server API in Rust), buttons `Website`, `Documentation`.
- A short note that this host answers the Chef API and points at `/version`
  and `/_status` in `<code>`.
- Footer: independent project, not affiliated with Progress or the Cinc
  Project, AGPL-3.0-only. One line, no numbers, no dates.

Copy stays within the facts already published on the homepage. MCP is
mentioned as `experimental` or not at all.

### Favicon

`landing/favicon.svg` is the single source. It is the mark from
`Logo.astro` — body, door, dial, two feet, gradient roof — redrawn at 64×64 on
the `--ink` panel with `rx="16"`, using the gradient for the roof and
`#f7f5ef` for the details, so it reads at 16 px. `website/homepage/public/favicon.svg`
and `website/docs/static/img/favicon.svg` become byte-identical copies; the
docs navbar logo picks the file up through its existing `img/favicon.svg`
reference, the homepage through its existing `<link rel="icon">`.

### Sites

`Base.astro` gets the CSP as `<meta http-equiv="Content-Security-Policy">`
directly after `charset`, plus `X-Content-Type-Options` and
`X-XSS-Protection` metas and a `Referrer-Policy` meta. The homepage policy has
to allow what the page already does: `script-src 'self' 'unsafe-inline'
https://www.youtube.com`, `frame-src https://www.youtube-nocookie.com`,
`img-src 'self' data:`, `connect-src 'self' https://www.youtube-nocookie.com`,
`style-src 'self' 'unsafe-inline'`, `font-src 'self'`, `base-uri 'self'`,
`form-action 'self'`, `frame-ancestors 'none'`.

The docs policy is set the same way through `headTags` in
`docusaurus.config.ts`, with `'unsafe-inline'` for script and style because
Docusaurus hydrates inline.

`website/homepage/README.md` loses its "aspirational until a header layer
exists" paragraph and documents what is actually set, and that
`X-Frame-Options` cannot be expressed in a meta tag.

### Documentation

- `AGENTS.md`: `crates/microwave-api/assets/` in the layout table; a Rust note
  that browser requests fall to the static page and that the header block and
  favicon are shared with the sites.
- `docs/deployment.md`: a short section next to TLS — the page exists, it is
  served to requests with no version or signature headers, the headers above
  apply to static responses only, and a reverse proxy in front may override
  them. It also notes that headerless `curl` examples now get HTML unless they
  send `X-Chef-Version` or `X-Cinc-Version`.

## Risks

- A site redesign changes `components.css` under the server page. The embedded
  path keeps it compiling; the Playwright spec catches layout breakage, and a
  visual regression is a one-file fix, not a from-scratch rewrite.
- The browser rule is broad, so an operator typing `/organizations/x` gets the
  page and never a JSON error. That is upstream behavior; `mcp` is exempt so
  agents keep their endpoint.
- `'unsafe-inline'` on the published sites means the site CSP is weaker than
  the server one. Documented, not hidden.
- Pages ignoring meta `frame-ancestors` in some browsers is a known gap; the
  server sets the real header.
- The broad rule changes what unsigned `curl` gets. The signing-guard keeps
  complete signatures on the API, but an unsigned scripted call now sees HTML.
  Documented in the deployment guide.

## Tests

Rust, `crates/microwave-api/tests/landing_test.rs`:

- `GET /` unsigned, no version header → `200`, `text/html`, the website link,
  the icon link, all six headers with the exact CSP, and the `Vary` value.
- Same with `X-Chef-Version: 19.0.0` → JSON `401`.
- `X-Cinc-Version` alone → the existing `400`/`200` matrix; both headers →
  `X-Chef-Version` decides.
- A complete signature without any version header (`support::signed_request`
  unchanged) → `200` JSON on `/users`, not HTML.
- `/nope` headerless → `200` page; with a version header → JSON `404`.
- Exempt paths headerless: `/version` plain text, `/_status` JSON,
  `/organizations/o/mcp` not HTML.
- `/index.html`, `/favicon.svg`, each `/css/*`, each `/fonts/*` → `200` with
  correct content type, with and without a version header; `/css/nope.css` and
  `/fonts/nope.woff2` → `404`.
- `HEAD /` → `200`, empty body, headers present.
- Maintenance mode on: headerless → `200` page, with a version header → `503`.
- API responses carry none of the six headers.
- Favicon copies: `assets/landing/favicon.svg`,
  `website/homepage/public/favicon.svg` and
  `website/docs/static/img/favicon.svg` are byte-identical (read through
  `CARGO_MANIFEST_DIR/../..`).

Website:

- `tests/content/site.test.mjs`: the CSP meta exists with `frame-ancestors
  'none'`, the icon link points at the favicon, the three hardening metas
  exist.
- `tests/e2e/landing.spec.ts`: the `webServer` command additionally copies
  `crates/microwave-api/assets/landing/index.html` to `.preview/index.html`,
  `landing.css` and `fonts.css` to `.preview/css/`, the site stylesheets to the
  same directory, and the favicon/fonts to `.preview/`; the spec navigates to
  `/` (no base path, those URLs are root-absolute) and asserts the stylesheet
  applied (computed `body` background is the ink color), one `h1`, and no
  horizontal overflow at 320/375/768/900/1440 px. CSP *headers* cannot be
  checked there because the preview is a bare `python3 -m http.server`; the
  meta tags are checked in the content test instead.
- The existing axe and console-error runs must stay green with the site CSP
  active.

Docs: a `node --test` spec asserting the built `build/index.html` carries the
CSP, the hardening metas and `img/favicon.svg`; wired into `pages.yml` after
the build.

## Open items left to the builder

- Final wording of the page copy within the facts above.
- Whether the exempt-path predicate should live next to the other `is_*_path`
  helpers or in `landing.rs`.
- The exact `landing.css` rules; `components.css` is marketing-tuned, so the
  hero needs tighter spacing for a single screen.
- Whether font preloads earn their two extra tags for a page this small.
