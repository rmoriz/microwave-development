# Server landing page, security headers, shared favicon — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Requests without a client marker get a static, embedded landing page
in the homepage's design, with a hardening header block, and one favicon shared
by server, homepage and docs.

**Architecture:** A `landing` module in `microwave-api` embeds HTML, CSS, the
favicon and two woff2 files with `include_str!`/`include_bytes!`, serves them
from the outer router with the header block, and an early branch in
`authenticate_request` answers browser-like requests with the page. The site
stylesheets are embedded straight from `website/homepage/src/styles/`, so the
page shares the site's design instead of a copy. Homepage and docs get the same
policy as `<meta>` tags, because GitHub Pages sets its own headers.

**Tech Stack:** Rust, axum, tower; Astro + `node --test` + Playwright on the
homepage; Docusaurus `headTags` on the docs.

**Spec:** `docs/superpowers/specs/2026-11-02-server-landing-page-design.md`

## Global Constraints

- Copy rules: English, playful cooking angle, no invented metrics or
  certifications, MCP always `experimental`, writes on by default and switched
  off with `writes = false`, never `AI-powered`.
- Page works with JavaScript disabled, no horizontal overflow at 320/375/768/900/1440 px,
  exactly one `h1`, sane heading order, no `'unsafe-inline'` in its CSP.
- Website URL is `https://rmoriz.github.io/microwave-development/`, in one
  constant.
- Static responses carry exactly:
  `Content-Security-Policy: default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
  `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`,
  `X-XSS-Protection: 1`, `Referrer-Policy: no-referrer`.
- Headerless page only when neither `X-Chef-Version` nor `X-Cinc-Version` nor a
  complete signature is present; exempt paths never get the page.
- No new Rust dependency, no runtime asset path, no Node in `cargo build`.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets` stay clean;
  internal URLs on the sites go through `withBase()`.

## Review Focus

1. A request with only *some* `X-Ops-*` headers sends a partial signature, not a
   browser — it must keep its JSON `401`, not receive HTML.
2. `/id/...` is oc-id's own login surface, not `location /` — a headerless
   browser must reach it, never the page.
3. A trailing slash on a static path (`/css/tokens.css/`) must still work,
   because trailing-slash normalization runs before the middleware.
4. A proxy must not cache the page for clients: `Vary: X-Chef-Version,
   X-Cinc-Version` on the page response.
5. `HEAD /` must return the status and headers with an empty body, not the
   document.

---

### Task 1: Embedded assets, page, and the static routes

**Files:**
- Create: `crates/microwave-api/assets/landing/index.html`
- Create: `crates/microwave-api/assets/landing/landing.css`
- Create: `crates/microwave-api/assets/landing/fonts.css`
- Create: `crates/microwave-api/assets/landing/favicon.svg`
- Create: `crates/microwave-api/assets/fonts/inter-latin.woff2` (copy of
  `website/homepage/node_modules/@fontsource-variable/inter/files/inter-latin-wght-normal.woff2`)
- Create: `crates/microwave-api/assets/fonts/space-grotesk-latin.woff2` (copy of
  `website/homepage/node_modules/@fontsource-variable/space-grotesk/files/space-grotesk-latin-wght-normal.woff2`)
- Create: `crates/microwave-api/assets/fonts/OFL.txt` (the `LICENSE` text of
  both `@fontsource-variable` packages, with the package versions)
- Create: `crates/microwave-api/src/landing.rs`
- Modify: `crates/microwave-api/src/lib.rs` (module list after `keys`, outer
  `router()` next to `/version`)
- Test: `crates/microwave-api/tests/landing_test.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub const WEBSITE_URL: &str`
  - `pub const CSS_FILES: [&str; 5]`
  - `pub const FONT_FILES: [&str; 2]`
  - `pub fn page() -> axum::response::Response`
  - `pub fn stylesheet(name: &str) -> Option<axum::response::Response>`
  - `pub fn font(name: &str) -> Option<axum::response::Response>`
  - `pub fn favicon() -> axum::response::Response`
  - `pub fn not_found() -> axum::response::Response`

- [ ] **Step 1: Write the failing test**

```rust
// crates/microwave-api/tests/landing_test.rs
mod support;

use axum::body::Body;
use axum::http::{header, HeaderName, HeaderValue, Request, StatusCode};
use tower::ServiceExt;

const EXPECTED_CSP: &str = "default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn headerless(method: &str, uri: &str) -> Request<Body> {
    Request::builder().method(method).uri(uri).body(Body::empty()).unwrap()
}

fn assert_static_headers(response: &axum::response::Response, content_type: &str) {
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], content_type);
    assert_eq!(headers["content-security-policy"], EXPECTED_CSP);
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["x-xss-protection"], "1");
    assert_eq!(headers["referrer-policy"], "no-referrer");
}

#[tokio::test]
async fn index_page_carries_the_header_block() {
    let response = support::app(support::state_with_pivotal())
        .oneshot(headerless("GET", "/index.html"))
        .await
        .unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
}

#[tokio::test]
async fn every_stylesheet_and_font_is_served_by_exact_name() {
    let app = support::app(support::state_with_pivotal());
    for (path, content_type) in [
        ("/css/tokens.css", "text/css; charset=utf-8"),
        ("/css/global.css", "text/css; charset=utf-8"),
        ("/css/components.css", "text/css; charset=utf-8"),
        ("/css/fonts.css", "text/css; charset=utf-8"),
        ("/css/landing.css", "text/css; charset=utf-8"),
        ("/fonts/inter-latin.woff2", "font/woff2"),
        ("/fonts/space-grotesk-latin.woff2", "font/woff2"),
        ("/favicon.svg", "image/svg+xml"),
    ] {
        let response = app.clone().oneshot(headerless("GET", path)).await.unwrap();
        assert_static_headers(&response, content_type);
    }
}

#[tokio::test]
async fn unknown_static_names_are_404_with_the_header_block() {
    let app = support::app(support::state_with_pivotal());
    for path in ["/css/nope.css", "/fonts/nope.woff2"] {
        let response = app.clone().oneshot(headerless("GET", path)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    }
}

#[tokio::test]
async fn a_versions_header_does_not_suppress_static_files() {
    let app = support::app(support::state_with_pivotal());
    let mut request = headerless("GET", "/css/tokens.css");
    request.headers_mut().insert(
        HeaderName::from_static("x-chef-version"),
        HeaderValue::from_static("19.0.0"),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_static_headers(&response, "text/css; charset=utf-8");
}

#[tokio::test]
async fn a_trailing_slash_still_reaches_the_stylesheet() {
    let app = support::app(support::state_with_pivotal());
    let response = app.oneshot(headerless("GET", "/css/tokens.css/")).await.unwrap();
    assert_static_headers(&response, "text/css; charset=utf-8");
}

#[tokio::test]
async fn head_returns_headers_without_a_body() {
    let app = support::app(support::state_with_pivotal());
    let response = app.oneshot(headerless("HEAD", "/index.html")).await.unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty());
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p microwave-api --test landing_test`
Expected: the file compiles and every test fails on `404` / missing
`content-security-policy`, because nothing is mounted yet.

- [ ] **Step 3: Write the assets**

`index.html`: `<html lang="en">`, `<title>Microwave Server</title>`, a
one-sentence `<meta name="description">`, `<link rel="icon" type="image/svg+xml"
href="/favicon.svg">`, links to the five stylesheets in the order tokens,
global, components, fonts, landing, then:

```html
<body>
  <header class="site-header">
    <div class="container nav-shell">
      <a class="logo" href="https://rmoriz.github.io/microwave-development/" aria-label="Microwave Server home">
        <!-- the inline SVG mark, copied from website/homepage/src/components/Logo.astro -->
        <span class="logo-word">microwave</span>
      </a>
    </div>
  </header>
  <main id="main" class="container">
    <section class="hero landing-hero">
      <p class="eyebrow">Independent · Open source · Rust</p>
      <h1>This is a <span class="hero-accent">Microwave server.</span></h1>
      <p class="hero-sub"><!-- one sentence: independent Chef Infra Server API implementation --></p>
      <div class="hero-actions">
        <a class="button button-primary" href="https://rmoriz.github.io/microwave-development/">Website</a>
        <a class="button button-ghost" href="https://rmoriz.github.io/microwave-development/docs/">Documentation</a>
        <a class="button button-ghost" href="https://github.com/rmoriz/microwave-development">GitHub</a>
      </div>
      <p class="fine-print">This host answers the Chef API. Version and health live
        at <code>/version</code> and <code>/_status</code>.</p>
    </section>
  </main>
  <footer class="site-footer"><div class="container">
    <p class="footer-disclaimer">An independent project, not affiliated with
      Progress Software Corporation or the Cinc Project. AGPL-3.0-only.</p>
  </div></footer>
</body>
```

`landing.css`: only what a single screen needs — `.landing-hero` padding and
`min-height`, and `.hero h1` font-size scaled down (the site's `7rem` is for a
landing hero). `fonts.css`: two `@font-face` blocks, families `'Inter Variable'`
and `'Space Grotesk Variable'`, `src: url('/fonts/<file>') format('woff2')`,
`font-weight: 100 900`, `font-display: swap`.

`favicon.svg`: 64×64, `rx="16"` background `#2a4d6e`, the mark's body, door,
dial and two feet in `#f7f5ef`, the roof line in the gradient
`#ff3ea5 → #ffe14d → #4dff9e → #3ddcff`, so it reads at 16 px.

Two `<link rel="preload" as="font" type="font/woff2" crossorigin>` tags for the
fonts are optional; add them only if the first paint visibly flashes.

- [ ] **Step 4: Implement `landing.rs`**

```rust
pub const WEBSITE_URL: &str = "https://rmoriz.github.io/microwave-development/";
pub const CSS_FILES: [&str; 5] = ["tokens.css", "global.css", "components.css", "fonts.css", "landing.css"];
pub const FONT_FILES: [&str; 2] = ["inter-latin.woff2", "space-grotesk-latin.woff2"];

pub fn page() -> Response;                    // text/html; charset=utf-8, no-cache
pub fn stylesheet(name: &str) -> Option<Response>; // text/css; charset=utf-8, public, max-age=3600
pub fn font(name: &str) -> Option<Response>;  // font/woff2, same caching
pub fn favicon() -> Response;                 // image/svg+xml, same caching
pub fn not_found() -> Response;               // text/plain, 404
```

`page` embeds `include_str!("../assets/landing/index.html")`. The three site
stylesheets come from `include_str!("../../../website/homepage/src/styles/tokens.css")`
(and `global`, `components`); `fonts.css` and `landing.css` come from the
assets directory. `favicon` and `font` use `include_bytes!`. Every response
passes through one private `fn with_security_headers(response: Response) ->
Response` that inserts the five headers from the spec.

- [ ] **Step 5: Mount the routes**

In `crates/microwave-api/src/lib.rs`, add `pub mod landing;` and, on the outer
router next to `/version` (outside the signed middleware):

```rust
.route("/index.html", get(|| async { landing::page() }))
.route("/favicon.svg", get(|| async { landing::favicon() }))
.route("/css/{file}", get(stylesheet_handler))
.route("/fonts/{file}", get(font_handler))
```

with

```rust
async fn stylesheet_handler(Path(file): Path<String>) -> Response {
    landing::stylesheet(&file).unwrap_or_else(landing::not_found)
}
async fn font_handler(Path(file): Path<String>) -> Response {
    landing::font(&file).unwrap_or_else(landing::not_found)
}
```

- [ ] **Step 6: Run the tests and the linters**

Run: `cargo test -p microwave-api --test landing_test`
Expected: PASS.
Run: `cargo fmt --all && cargo clippy -p microwave-api --all-targets`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/microwave-api/assets crates/microwave-api/src/landing.rs crates/microwave-api/src/lib.rs crates/microwave-api/tests/landing_test.rs
git commit -m "feat(api): embed a landing page with a hardening header block"
```

---

### Task 2: Confirm what Cinc Client actually sends

**Files:**
- Modify: `docs/superpowers/plans/2026-11-02-server-landing-page.md` (record
  the outcome under this task)

**Interfaces:**
- Consumes: nothing.
- Produces: the fact the next task relies on — whether a real Cinc Client
  converge sends `X-Cinc-Version`, `X-Chef-Version`, or neither.

- [ ] **Step 1: Capture the headers of a real converge**

From `docker-e2e/`, build and start the stack for one node, then capture the
client's requests:

```bash
docker compose build node-debian
docker compose up -d node-debian
docker compose exec node-debian sh -c 'command -v tcpdump >/dev/null || (apt-get update && apt-get install -y tcpdump)'
docker compose exec node-debian tcpdump -A -s0 -i any 'tcp port 8000 and tcp[((tcp[12]&0xf0)>>2):4] = 0x47455420 or tcp port 8000' | grep -ai 'cinc-version\|chef-version'
```

- [ ] **Step 2: Record the outcome and clean up**

Append one line to this task, either
`Outcome: Cinc Client sends X-Cinc-Version.` or
`Outcome: Cinc Client sends no version header; the signing guard carries the converge.`
Then tear the stack down: `docker compose down -v --rmi local`.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/plans/2026-11-02-server-landing-page.md
git commit -m "docs(plans): record the Cinc client version header"
```

---

### Task 3: Browser branch and `X-Cinc-Version`

**Files:**
- Modify: `crates/microwave-api/src/middleware.rs` (top of
  `authenticate_request`; the `/_status`… block near the auth fallthrough;
  new predicates next to `has_signing_headers`)
- Test: `crates/microwave-api/tests/landing_test.rs` (append)

**Interfaces:**
- Consumes: `landing::page()` from Task 1.
- Produces: nothing later tasks call.

- [ ] **Step 1: Write the failing tests**

Add `use microwave_crypto::SignVersion;` to the test file's imports, then
append:

```rust
#[tokio::test]
async fn the_root_answers_a_browser_with_the_page() {
    let response = support::app(support::state_with_pivotal())
        .oneshot(headerless("GET", "/"))
        .await
        .unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
    assert_eq!(response.headers()["vary"], "X-Chef-Version, X-Cinc-Version");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("https://rmoriz.github.io/microwave-development/"));
    assert!(html.contains(r#"<link rel="icon""#));
    assert_eq!(html.matches("<h1").count(), 1);
}

#[tokio::test]
async fn an_unknown_path_answers_a_browser_with_the_page() {
    let response = support::app(support::state_with_pivotal())
        .oneshot(headerless("GET", "/nope"))
        .await
        .unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
}

#[tokio::test]
async fn a_chef_version_header_keeps_the_api_behavior() {
    let app = support::app(support::state_with_pivotal());
    for (path, expected) in [("/", StatusCode::UNAUTHORIZED), ("/nope", StatusCode::NOT_FOUND)] {
        let mut request = headerless("GET", path);
        request.headers_mut().insert(
            HeaderName::from_static("x-chef-version"),
            HeaderValue::from_static("19.0.0"),
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected, "{path}");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    }
}

#[tokio::test]
async fn x_cinc_version_runs_the_same_minimum_matrix() {
    let app = support::app(support::state_with_pivotal());
    for (version, expected) in [
        ("9.0.1", StatusCode::BAD_REQUEST),
        ("0.9.1", StatusCode::BAD_REQUEST),
        ("0.10.1", StatusCode::OK),
        ("19.0.0", StatusCode::OK),
    ] {
        let mut request = support::signed_request("GET", "/users", b"", SignVersion::V1_3, "pivotal");
        request.headers_mut().insert(
            HeaderName::from_static("x-cinc-version"),
            HeaderValue::from_str(version).unwrap(),
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected, "X-Cinc-Version {version}");
    }
}

#[tokio::test]
async fn chef_version_wins_over_cinc_version() {
    let app = support::app(support::state_with_pivotal());
    let mut request = support::signed_request("GET", "/users", b"", SignVersion::V1_3, "pivotal");
    request.headers_mut().insert(
        HeaderName::from_static("x-chef-version"),
        HeaderValue::from_static("9.0.1"),
    );
    request.headers_mut().insert(
        HeaderName::from_static("x-cinc-version"),
        HeaderValue::from_static("19.0.0"),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_partial_signature_is_not_a_browser() {
    let app = support::app(support::state_with_pivotal());
    let mut request = headerless("GET", "/users");
    request.headers_mut().insert(
        HeaderName::from_static("x-ops-userid"),
        HeaderValue::from_static("pivotal"),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(response.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
}

#[tokio::test]
async fn exempt_paths_never_serve_the_page() {
    let app = support::app(support::state_with_pivotal());
    for path in ["/_status", "/_stats", "/server_api_version", "/id", "/organizations/o/mcp"] {
        let response = app.clone().oneshot(headerless("GET", path)).await.unwrap();
        assert_ne!(
            response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn maintenance_mode_still_shows_the_page() {
    let state = support::state_with_pivotal();
    state.sqlite().set_maintenance_enabled(true).unwrap();
    let app = support::app(state);
    let response = app.clone().oneshot(headerless("GET", "/")).await.unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
    let mut request = headerless("GET", "/");
    request.headers_mut().insert(
        HeaderName::from_static("x-chef-version"),
        HeaderValue::from_static("19.0.0"),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p microwave-api --test landing_test`
Expected: FAIL — `/` answers `401` JSON, `X-Cinc-Version` is ignored.

- [ ] **Step 3: Add the predicates**

Near `has_signing_headers`:

```rust
/// The client version headers, `X-Chef-Version` first.
fn client_version(headers: &HeaderMap) -> Option<&str> {
    ["x-chef-version", "x-cinc-version"]
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
}

/// Paths upstream answers from their own nginx `location`, before
/// `location /` can serve `index.html`.
fn is_nginx_only_path(path: &str) -> bool {
    path == "/_status"
        || path == "/_stats"
        || path == "/server_api_version"
        || path == "/id"
        || path.starts_with("/id/")
        || path.starts_with("/bookshelf/")
        || validate_signed_path(path).is_some()
        || is_required_recipe_path(path)
        || is_data_collector_path(path)
        || is_mcp_path(path)
        || is_compliance_proxy_path(path)
}

/// No client marker and no complete signature, on a path `location /` owns.
fn serves_landing_page(path: &str, headers: &HeaderMap) -> bool {
    client_version(headers).is_none() && !has_signing_headers(headers) && !is_nginx_only_path(path)
}
```

- [ ] **Step 4: Branch in `authenticate_request`**

Replace the `x-chef-version` lookup with `client_version(request.headers())`,
and insert the branch directly after `parse_api_version` and before the version
check:

```rust
if serves_landing_page(&path, request.headers()) {
    return landing::page();
}
```

Add `use crate::landing;` to the module's imports. Add `Vary` and the two
version header names to the page response in `landing::page()`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p microwave-api --test landing_test`
Expected: PASS.
Run: `cargo test -p microwave-api`
Expected: PASS — no existing test relies on a signed request without a version
header receiving HTML.

- [ ] **Step 6: Commit**

```bash
git add crates/microwave-api/src/middleware.rs crates/microwave-api/src/landing.rs crates/microwave-api/tests/landing_test.rs
git commit -m "feat(api): serve the landing page to browser requests"
```

---

### Task 4: Favicon copies and the drift test

**Files:**
- Modify: `website/homepage/public/favicon.svg`
- Modify: `website/docs/static/img/favicon.svg`
- Test: `crates/microwave-api/tests/landing_test.rs` (append)

**Interfaces:**
- Consumes: `crates/microwave-api/assets/landing/favicon.svg` from Task 1.
- Produces: the three byte-identical copies the content tests later assert.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_surface_serves_the_same_favicon() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = std::fs::read(root.join("crates/microwave-api/assets/landing/favicon.svg")).unwrap();
    for copy in [
        "website/homepage/public/favicon.svg",
        "website/docs/static/img/favicon.svg",
    ] {
        assert_eq!(std::fs::read(root.join(copy)).unwrap(), source, "{copy}");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p microwave-api --test landing_test every_surface`
Expected: FAIL on the first copy.

- [ ] **Step 3: Copy the asset over both files**

```bash
cp crates/microwave-api/assets/landing/favicon.svg website/homepage/public/favicon.svg
cp crates/microwave-api/assets/landing/favicon.svg website/docs/static/img/favicon.svg
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p microwave-api --test landing_test every_surface`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add website/homepage/public/favicon.svg website/docs/static/img/favicon.svg crates/microwave-api/tests/landing_test.rs
git commit -m "feat(website): use the logo mark as the single favicon"
```

---

### Task 5: Playwright coverage for the page

**Files:**
- Create: `website/homepage/scripts/stage-landing.mjs`
- Create: `website/homepage/tests/e2e/landing.spec.ts`
- Modify: `website/homepage/playwright.config.ts` (webServer command)

**Interfaces:**
- Consumes: the asset paths from Task 1.
- Produces: nothing later tasks call.

- [ ] **Step 1: Write the staging script**

`stage-landing.mjs` copies, from `../../crates/microwave-api/assets/`:
`landing/index.html` → `.preview/index.html`, `landing/fonts.css` and
`landing/landing.css` → `.preview/css/`, `landing/favicon.svg` →
`.preview/favicon.svg`, `fonts/*.woff2` → `.preview/fonts/`, and the three site
stylesheets from `src/styles/` → `.preview/css/`. It creates directories and
exits non-zero when a source is missing.

- [ ] **Step 2: Write the failing spec**

```ts
import { expect, test } from '@playwright/test';

const widths = [320, 375, 768, 900, 1440];

test('the landing page renders with the site design', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('h1')).toHaveCount(1);
  const background = await page
    .locator('body')
    .evaluate((body) => getComputedStyle(body).backgroundColor);
  expect(background).toBe('rgb(42, 77, 110)');
  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow, `overflow at ${width}px`).toBeLessThanOrEqual(1);
  }
});
```

- [ ] **Step 3: Wire it into the preview server**

In `playwright.config.ts`, extend the webServer command with
`&& node scripts/stage-landing.mjs` after the `cp -R dist/. ${previewDir}/`
step, and leave the `url` as the site URL.

- [ ] **Step 4: Run it**

Run: `cd website/homepage && npx playwright test tests/e2e/landing.spec.ts`
Expected: PASS; the only likely failure is an overflow assertion, which is
fixed in `landing.css` and not in the spec.

- [ ] **Step 5: Commit**

```bash
git add website/homepage/scripts/stage-landing.mjs website/homepage/tests/e2e/landing.spec.ts website/homepage/playwright.config.ts
git commit -m "test(website): render the landing page in the browser suite"
```

---

### Task 6: Homepage meta policy

**Files:**
- Modify: `website/homepage/src/layouts/Base.astro` (directly after `<meta charset>`)
- Modify: `website/homepage/tests/content/site.test.mjs`
- Modify: `website/homepage/README.md` (the "Cache headers" section)

**Interfaces:**
- Consumes: nothing.
- Produces: the meta values the docs task mirrors.

- [ ] **Step 1: Write the failing test**

Append to `site.test.mjs`, in the existing `$`-based style:

```js
test('the head carries the security metas', () => {
  const metas = new Map(
    $('meta[http-equiv]')
      .toArray()
      .map((el) => [$(el).attr('http-equiv').toLowerCase(), $(el).attr('content')]),
  );
  const csp = metas.get('content-security-policy');
  assert.ok(csp, 'Content-Security-Policy meta');
  assert.match(csp, /frame-ancestors 'none'/);
  assert.match(csp, /script-src 'self' 'unsafe-inline' https:\/\/www\.youtube\.com/);
  assert.match(csp, /frame-src https:\/\/www\.youtube-nocookie\.com/);
  assert.equal(metas.get('x-content-type-options'), 'nosniff');
  assert.equal(metas.get('x-xss-protection'), '1');
  assert.equal(metas.get('referrer-policy'), 'no-referrer');
  assert.equal($('link[rel="icon"]').attr('href'), `${BASE_PATH}/favicon.svg`);
});
```

Use the file's existing imports for `test`, `assert`, `$` and the base path.

- [ ] **Step 2: Run and watch it fail**

Run: `cd website/homepage && npm run build && node --test tests/content/site.test.mjs`
Expected: FAIL — no `meta[http-equiv]` exists.

- [ ] **Step 3: Add the metas to `Base.astro`**

```astro
<meta http-equiv="Content-Security-Policy" content="default-src 'self'; script-src 'self' 'unsafe-inline' https://www.youtube.com; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self' https://www.youtube-nocookie.com; base-uri 'self'; form-action 'self'; frame-src https://www.youtube-nocookie.com; frame-ancestors 'none'" />
<meta http-equiv="X-Content-Type-Options" content="nosniff" />
<meta http-equiv="X-XSS-Protection" content="1" />
<meta http-equiv="Referrer-Policy" content="no-referrer" />
```

- [ ] **Step 4: Run the whole homepage suite**

Run: `cd website/homepage && npm test`
Expected: PASS, including axe and the console-error check. Fix violations by
narrowing the policy (for example a missing `img-src` origin), never by
dropping `frame-ancestors`.

- [ ] **Step 5: Update the README**

Replace the "aspirational until a header layer exists" paragraph with what is
actually set: the four metas, the reason (`X-Frame-Options` cannot be expressed
in a meta tag, so `frame-ancestors 'none'` carries that job), and that Pages
still sets its own real headers.

- [ ] **Step 6: Commit**

```bash
git add website/homepage/src/layouts/Base.astro website/homepage/tests/content/site.test.mjs website/homepage/README.md
git commit -m "feat(website): add the security policy metas"
```

---

### Task 7: Docs policy and favicon test

**Files:**
- Modify: `website/docs/docusaurus.config.ts` (`headTags`)
- Create: `website/docs/tests/head.test.mjs`
- Modify: `website/docs/package.json` (`"test"` script)
- Modify: `.github/workflows/pages.yml` (a `npm test` step after the docs build)

**Interfaces:**
- Consumes: the meta values from Task 6.
- Produces: nothing.

- [ ] **Step 1: Write the failing test**

```js
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const html = readFileSync(new URL('../build/index.html', import.meta.url), 'utf8');

test('the docs head carries the security metas and the favicon', () => {
  const csp = /<meta http-equiv="Content-Security-Policy" content="([^"]+)"/.exec(html);
  assert.ok(csp, 'Content-Security-Policy meta');
  assert.match(csp[1], /frame-ancestors 'none'/);
  assert.match(html, /<meta http-equiv="X-Content-Type-Options" content="nosniff"/);
  assert.match(html, /<meta http-equiv="X-XSS-Protection" content="1"/);
  assert.match(html, /<link rel="icon"[^>]*img\/favicon\.svg/);
});
```

- [ ] **Step 2: Run and watch it fail**

Run: `cd website/docs && npm run build && node --test tests/`
Expected: FAIL on the missing CSP.

- [ ] **Step 3: Add `headTags`**

In `docusaurus.config.ts`:

```ts
headTags: [
  {
    tagName: 'meta',
    attributes: {
      'http-equiv': 'Content-Security-Policy',
      content:
        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; base-uri 'self'; form-action 'self'; frame-ancestors 'none'",
    },
  },
  { tagName: 'meta', attributes: { 'http-equiv': 'X-Content-Type-Options', content: 'nosniff' } },
  { tagName: 'meta', attributes: { 'http-equiv': 'X-XSS-Protection', content: '1' } },
  { tagName: 'meta', attributes: { 'http-equiv': 'Referrer-Policy', content: 'no-referrer' } },
],
```

- [ ] **Step 4: Run the build and the test**

Run: `cd website/docs && npm run build && node --test tests/`
Expected: PASS.

- [ ] **Step 5: Wire it into CI**

Add `"test": "node --test tests/"` to the docs `package.json` scripts and a
`- run: npm test` step after the docs build step in `pages.yml`.

- [ ] **Step 6: Commit**

```bash
git add website/docs/docusaurus.config.ts website/docs/tests/head.test.mjs website/docs/package.json .github/workflows/pages.yml
git commit -m "feat(docs): add the security policy metas"
```

---

### Task 8: Documentation and full suites

**Files:**
- Modify: `AGENTS.md` (layout table, Rust notes)
- Modify: `docs/deployment.md` (next to the TLS section)

**Interfaces:**
- Consumes: the finished behavior.
- Produces: nothing.

- [ ] **Step 1: Document the surface**

`AGENTS.md`: a layout row for `crates/microwave-api/assets/` (page, icons,
fonts — embedded, shared with the sites) and verified by `cargo test
--workspace`; a Rust note: headerless, unsigned requests on non-nginx paths get
the page, the header block applies only to static responses, and the favicon is
byte-identical across the three surfaces.

`docs/deployment.md`: a section next to TLS covering what a browser gets, the
five headers and their scope, that a reverse proxy in front may override them,
and that a headerless `curl` now receives HTML unless it sends
`X-Chef-Version` or `X-Cinc-Version`.

- [ ] **Step 2: Run the documentation checks**

Run: `python3 scripts/check-claims.py`
Expected: no output.

- [ ] **Step 3: Run both full suites**

Run: `cargo test --workspace`
Expected: PASS.
Run: `cd website/homepage && npm test && cd ../docs && npm run build && npm test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md docs/deployment.md
git commit -m "docs: describe the landing page and its header block"
```
