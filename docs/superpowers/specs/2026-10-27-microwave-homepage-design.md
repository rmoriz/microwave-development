# Microwave homepage — design

- **Date:** 2026-10-27
- **Status:** Draft for approval
- **Location:** `website/homepage`
- **Domain:** `https://microwave.io`

## 1. Intent

Build the public landing page for Microwave Server: a sales-grade page that
also survives a technical evaluation. The page has to explain the product in
seconds and give an engineer enough evidence to start a real trial. A separate
documentation site will come later; until then the homepage links into the
repository docs.

The page is also a sales tool. A salesperson should be able to send the link
without a deck, and the page should answer the buyer's first questions without
requiring a call.

## 2. What the product is

Microwave Server is an independent Rust implementation of the Chef Infra Server
API, designed as a drop-in replacement for existing clients and operations
tooling. It is not affiliated with or endorsed by Progress Software
Corporation.

The parts that matter on the page:

- Rust, one server binary, no Ruby runtime.
- SQLite by default; PostgreSQL supported through the same metadata contract.
- Local object store and embedded search by default; S3/Garage and
  OpenSearch/Elasticsearch supported.
- Chef-compatible signed requests, ACLs, cookbooks, policyfiles, search, data
  bags, OAuth/identity, and operations CLI.
- `microwave-ctl`, with a `chef-server-ctl` compatibility alias.
- Migration tooling for snapshots, knife backups, and live legacy servers.
- Cinc Client is the primary client target. The repository's Compose end-to-end
  stack installs Cinc Client 19 on Debian, Ubuntu, and Fedora. Stock
  `chef-client` 18/19 also converges with compatibility shims disabled.
- AGPL-3.0-only, free to self-host, with commercial support available.

## 3. Audience and jobs

**Primary buyer — platform / operations lead.** Often already running Chef or
Cinc, responsible for keeping configuration management alive. Wants to know:
Is this real? Is it safe to run? Will our cookbooks and pipelines keep working?
Who supports it if something breaks? What does it cost?

**Technical evaluator — senior engineer.** Wants architecture, API coverage,
storage choices, test evidence, migration mechanics, and an honest list of
gaps. Will judge the project by whether the page respects their time.

**Salesperson.** Wants one link that tells the story, establishes credibility,
and ends in a conversation. Does not want to explain a licensing model or a
Rust toolchain from scratch.

## 4. Positioning

**Headline:** “Get cooking without becoming a chef.”

**Supporting line:** “Microwave is an independent, open-source Chef Infra
Server API implementation written in Rust. Keep your cookbooks, clients, and
CLI. Run a simpler server, with the backing services you already trust.”

**Cinc angle:** Cinc Client gives teams a free, community-built client. Microwave
gives that client an independent server. This is a natural pairing, not a
merger: Cinc is a separate project with its own community and governance. The
page should say “works with Cinc Client” and link to `cinc.sh`, never imply an
official partnership.

**Promise:** the same recipes and tooling, a smaller operational surface, no
vendor lock-in, and optional commercial help.

### Messaging guardrails

- Never disparage Chef, Progress Software, or the Cinc Project. No pricing
  attacks, no “EOL” fear, no “cheaper than Chef” claims, no comparison table
  designed to make a competitor look bad.
- Sell the positive: independent, open source, Rust, maintained, free to
  self-host, commercial support when wanted.
- Respect trademarks. Use “Chef”, “Chef Infra”, and “Cinc” only to describe
  compatibility, never in the product name, logo, or domain. Do not use Chef or
  Cinc logos. Include the independence disclaimer in the footer.
- Every factual claim must be traceable to the repository or a public source.
  Link the evidence where it helps.
- Be explicit about limitations. An honest gap builds more trust than an
  “enterprise-grade” adjective.
- No invented customers, testimonials, logos, benchmarks, certifications, or
  support SLAs.

## 5. Goals, non-goals, anti-goals

### Goals

- Explain what Microwave is in under 10 seconds.
- Give a buyer enough confidence to book a conversation in under 60 seconds.
- Give an engineer enough detail to open the repository and start a trial.
- Make Cinc Client the obvious first integration.
- Establish the legal and licensing position clearly.
- Load fast, work on mobile, and meet WCAG 2.2 AA.

### Non-goals

- A full documentation site, blog, changelog, or knowledge base.
- Pricing tables, checkout, self-service accounts, or a customer portal.
- A CMS, authentication, dashboards, or server-side code.
- Customer stories or benchmark charts (none exist yet).
- Internationalization in the first version.
- Live chat or analytics.

### Anti-goals

- Looking like a generic enterprise SaaS template.
- Implying endorsement by Progress, Chef, or the Cinc Project.
- Overstating production readiness or hiding known gaps.
- Aggressive competitor positioning.
- Heavy animation, autoplay video, or tracking that slows the page down.
- A page that only engineers can love, or one that only sales can love.

## 6. Constraints

- Static site. No application server, database, or form backend.
- Contact happens through a `mailto:` link or an external scheduling link.
  The destination is a single configurable constant.
- The existing `docs/images/microwave-hero.png` is the hero artifact. Its use
  is assumed to be cleared by the project owner.
- English copy.
- Core content must work without JavaScript. JavaScript is progressive
  enhancement only.
- No external font, image, or analytics requests at runtime.
- Deployable as static files to any CDN or static host.
- The repository is AGPL-3.0-only; the site must state the license accurately
  without suggesting that the website content and the server software are the
  same work.

## 7. Approaches considered

### A. Campaign page — chosen

One long editorial page built around the headline. Dark stage, the rainbow
microwave as the hero artifact, short sections that alternate sales benefit and
hard technical proof, and expandable detail for evaluators.

**Why:** it makes the claim memorable, gives the salesperson a story, and can
still carry the evidence an engineer needs. The brand has a distinctive asset;
a generic layout would waste it.

**Trade-off:** more copy and design work than a template, and the technical
claims must stay precise or the whole page loses credibility.

### B. Product-sheet page

Conventional light SaaS layout: logo bar, three-column feature grid, comparison
table, testimonial placeholders, demo form.

**Why not:** safe and scannable, but forgettable. It sells the category rather
than this project, and it has no honest way to fill the social-proof slots yet.

### C. Terminal / docs-first page

Near-monochrome, code-heavy, numbers first, neon only as an accent.

**Why not:** excellent for engineers, too dry for the buyer, and it duplicates
the documentation site that is coming later.

**Decision:** A, with C's evidence density and B's scannability.

## 8. Page structure

Single page at `/`. Sticky navigation with anchor links.

### 8.1 Navigation

- Wordmark: “microwave” with a small microwave-window glyph.
- Links: Product, Compatibility, Under the hood, Support, FAQ.
- GitHub link.
- Primary button: “Talk to us”.

### 8.2 Hero

- Eyebrow: “Independent · Open source · Rust”
- H1: “Get cooking without becoming a chef.”
- Subhead: the supporting line from §4.
- Primary CTA: “Talk to us about support”.
- Secondary CTA: “Run it from source”, linking to the repository's documented
  run instructions. No install command on the page, because there is no
  published image or crate yet.
- Badges: “Cinc Client 19”, “Chef Infra Client 18/19”, “AGPL-3.0”.
- Hero image: the rainbow microwave, framed, with a small caption:
  “Artist's impression. The real thing is a single binary.”
- Background: near-black with a soft rainbow bloom behind the image, subtle
  grain, and a hairline grid that fades out.

### 8.3 Proof strip

Four verifiable numbers, each with a link to evidence:

| Number | Label | Evidence |
| --- | --- | --- |
| 3,284 | extended Pedant examples passing, 0 failures | `docs/pedant-extended-report.md` |
| 0 | compatibility shims needed for the strict `chef-client` converge | `docs/chef-client-e2e-report.md` |
| 3 | distros in the Cinc Client 19 Compose e2e stack | `docker-e2e/README.md` |
| 1 | binary to run; SQLite by default | `README.md` |

The Cinc claim is only promoted to “tested” after the e2e stack is run during
implementation. If it cannot be run, the wording becomes “the repository ships
an end-to-end stack covering…” and the stat is replaced.

### 8.4 “Cinc Client, meet Microwave.”

A dedicated section for the primary client target.

- Cinc gives you a free, community-built client. Microwave gives it an
  independent server.
- Keep your cookbooks, roles, data bags, search, policyfiles, and pipelines.
- Tested across Debian, Ubuntu, and Fedora with Cinc Client 19.
- Works with the `chef-client` wrapper and the existing `knife` workflow.
- Small print: Cinc is a separate project; Microwave is not affiliated with or
  endorsed by the Cinc Project.

### 8.5 “Same recipe. New kitchen.” — compatibility

Three columns, each with a short list:

**API** — organizations, users, clients, keys, nodes, roles, environments,
data bags, cookbooks, policyfiles, search, ACLs, OAuth/identity, data
collector, compliance proxy, validation endpoints.

**Auth** — `X-Ops-Sign` 1.0/1.1/1.3 canonicalization, RSA signing and
verification, key reassembly, recursive ACL and group checks.

**Tooling** — `knife`, `chef-client`, `cinc-client`, `microwave-ctl`, and the
`chef-server-ctl` compatibility alias.

A short note underneath: “API parity is a moving target; the compatibility
report and known gaps live in the repository.” Link to the README's HTTP route
parity section and the Pedant reports. There is no separate public
compatibility page yet; do not promise one.

### 8.5.1 “Known gaps” disclosure

A short, honest list under the technical sections, linked from the FAQ:

- No automatic `chef-server.rb` importer; configuration is translated with the
  deployment guide.
- `from-api` migration cannot copy password hashes or the admin flag; users
  need a password reset or external identity after that path.
- No LDAP/Active Directory bind and no FIPS-validated crypto mode.
- Embedded search is a relational scan, not an inverted index; larger
  installations should use OpenSearch/Elasticsearch.
- Multi-replica HA and automated failover are not validated.
- Only password-reset email is implemented; no invitation mail or durable
  outbox.

This is a trust device, not a confession. Keep it compact and link to the
README's “Remaining Work” section.

### 8.6 “Under the hood” — technical deep dive

An accessible accordion with six panels. Each panel has two or three short
paragraphs and a link to the relevant repository doc.

1. **One binary, two databases.** SQLite by default, PostgreSQL through the
   same metadata contract. Transparent document compression. Snapshot
   migration between backends.
2. **Auth that matches.** Signed-request canonicalization, RSA verification,
   legacy password hashes, recursive ACLs, and the default organization
   policy.
3. **Cookbooks and policy.** Sandboxes, bookshelf uploads, S3/Garage presigned
   URLs, cookbook resolution, policyfiles, and policy groups.
4. **Search your way.** Embedded relational search by default;
   OpenSearch/Elasticsearch for larger installations; reindexing and
   result-level ACL filtering.
5. **Deploy your way.** Docker, Compose, systemd, Debian package, direct TLS,
   `SIGHUP` certificate reload, external object store and search. Documented
   topologies are single-node or external-backend; multi-replica HA is not yet
   validated.
6. **Move in.** `microwave-migrate` snapshots, `knife ec backup` import, and
   the live legacy API crawler. There is no automatic `chef-server.rb`
   importer; configuration is translated using the deployment guide.

### 8.7 “From your current server” — migration

Two real paths, presented plainly:

**If you have a `knife ec backup`:** import the repository directly with
`microwave-migrate import-knife-backup`. It preserves users, password hashes
when the key dump contains them, organizations, clients, ACLs, cookbooks, and
cookbook blobs into a local object store. Cookbook artifacts, policies, OAuth
applications, search indexes, and `kvpair` secrets are not part of a knife
backup and are not imported.

**If you only have the live server:** crawl it with
`microwave-migrate from-api`. It reads organizations, users, clients, groups,
nodes, roles, environments, data bags, cookbooks, policies, and ACLs. The
legacy API does not expose password hashes or the admin flag, so those users
need a password reset or external identity after the move.

Then **repoint** clients and DNS at your pace; the API surface is the same.

Snapshot export/import is for moving a Microwave store between SQLite and
PostgreSQL, not for importing a stock Chef Infra Server.

A note: “No automatic `chef-server.rb` import. The configuration translation
guide covers the move.” Link to `docs/deployment.md` and the migration README.

### 8.8 “Free software. Real support.”

- Microwave Server is AGPL-3.0-only. Self-hosting is free. There is no seat
  count or licence fee.
- Commercial support is available for teams that want a named engineer,
  migration help, or a production review. Scope and terms are agreed directly.
- Three support cards, worded generically until the actual offer is confirmed:
  **Migration**, **Deployment review**, **Ongoing support**.
- Primary CTA: “Talk to us”.

No prices, tiers, SLAs, or response times until the owner supplies them.

### 8.9 FAQ

Eight questions, written for both buyer and engineer:

1. Is Microwave affiliated with Chef, Progress, or the Cinc Project?
2. Is this a fork of Chef Infra Server?
3. Does it work with Cinc Client?
4. Is it really free?
5. Can we migrate our existing server?
6. Is it production-ready?
7. What is missing today?
8. How do we get commercial support?

Answers are direct and link to evidence. “Production-ready” is answered
honestly: a working implementation with extensive test coverage, evaluated
against the customer's own runbooks; known gaps are documented.

### 8.10 Final CTA

“Bring your own cookbooks.” A “Run it from source” link into the repository,
“Talk to us”, and a GitHub link. A final line: “Free to self-host. Supported
when you want it.”

### 8.11 Footer

- Independence disclaimer: not affiliated with or endorsed by Progress
  Software Corporation, Chef Software, or the Cinc Project.
- Trademark notice: Chef, Chef Infra, and Chef Infra Server are trademarks of
  Progress Software Corporation. Cinc is a project of the Cinc community.
- License: Microwave Server is AGPL-3.0-only. Upstream source snapshots under
  `chef/` and `chef-server/` retain their original Apache-2.0 licensing.
- Links: GitHub, docs, license, security, privacy, imprint.
- Privacy: “No cookies, no analytics, no third-party requests.” The privacy
  page also names the hosting provider and its server-log retention once known.
- An imprint page with the operator's legal name, address, and contact details.
  Required for a German operator; the details must come from the owner, not be
  invented.
- Copyright line.

## 9. Visual design

**Mood:** a premium appliance in an open-source workshop. Editorial, warm, and
precise; playful only where the headline and hero image earn it.

**Palette**

- Ink: `#0a0a0b`
- Panel: `#121214`
- Text: `#f4f1e8`
- Muted: `#a3a39b`
- Hairline: `rgba(244, 241, 232, 0.12)`
- Gradient stops from the hero image: magenta `#ff3ea5`, orange `#ff8a3d`,
  yellow `#ffe14d`, green `#4dff9e`, cyan `#3ddcff`, violet `#8b5cff`.
- The gradient is an accent, never a background wash. Use it for a thin top
  rule, a highlighted word, the logo mark, and small details.

**Typography**

- Display: Space Grotesk Variable.
- Body: Inter Variable.
- Technical: JetBrains Mono Variable.
- Self-hosted through Fontsource; no Google Fonts requests.
- Display sizes: hero `clamp(3rem, 8vw, 7rem)`, section `clamp(2rem, 4vw, 3.5rem)`,
  body `1.0625rem` with `1.65` line height.

**Layout**

- Max width 1200px, 24px gutters, 12-column grid.
- Asymmetric hero: 55% copy, 45% image; stacks on mobile.
- Full-bleed proof band and CTA band.
- Vertical rhythm: 96px mobile, 128–160px desktop between sections.
- Hairline borders and generous negative space instead of cards everywhere.

**Components**

- Pill badges with a subtle gradient border.
- Inline code styling for package and command names; no install block.
- Stat cells with large mono numbers and small labels.
- Accordion using native `<details>` / `<summary>` with custom styling.
- Step cards with mono numerals.
- FAQ rows with plus/minus indicators.
- Sticky nav that gains a blurred background after scrolling.

**Motion**

- 200–500ms ease transitions.
- Fade-and-rise on section entry via `IntersectionObserver`; content is visible
  without JavaScript.
- Hero image has a very slow ambient float, paused for
  `prefers-reduced-motion`.
- No parallax, no autoplay, no cursor-following effects.

**Image treatment**

- Hero image in a rounded 16px frame with a rainbow bloom behind it and a
  subtle inner vignette.
- Responsive AVIF/WebP with PNG fallback; preload the hero.
- Target under 300KB for the largest hero variant.
- OG image: 1200×630 crop with the headline over a darkened hero image.

## 10. Technical design

**Stack:** Astro, static output, vanilla CSS, no UI framework. TypeScript only
where it earns its keep.

**Why Astro:** static HTML by default, built-in image optimization, component
structure for a long page, and a clean path to adding a docs site or blog later
without adopting a runtime.

**Repository layout**

```
website/homepage/
  package.json
  astro.config.mjs
  tsconfig.json
  README.md
  public/
    favicon.svg
    robots.txt
    site.webmanifest
  src/
    assets/
      microwave-hero.png
      og.png
    components/
      Nav.astro
      Hero.astro
      ProofStrip.astro
      CincSection.astro
      Compatibility.astro
      UnderTheHood.astro
      Migration.astro
      Support.astro
      FAQ.astro
      FinalCTA.astro
      Footer.astro
    layouts/
      Base.astro
    pages/
      index.astro
      404.astro
      privacy.astro
      imprint.astro
    styles/
      tokens.css
      global.css
      components.css
    data/
      site.ts
```

**Content and links** live in `src/data/site.ts`: contact email, GitHub URL,
docs base URL, legal details, and claim links. Changing the contact address or
repository URL is a one-line edit.

**JavaScript:** a single small module for the mobile menu and scroll reveals.
Accordions use native `<details>`. Target under 10KB gzipped.

**Fonts:** self-hosted variable WOFF2 from Fontsource. Subset to Latin.
`font-display: swap`.

**SEO**

- Title: “Microwave Server — Get cooking without becoming a chef.”
- Meta description focused on independent, open source, Rust, Cinc Client.
- Canonical `https://microwave.io/`.
- Open Graph and Twitter cards.
- `SoftwareApplication` JSON-LD with name, description, license, code
  repository, and operating system. No fake ratings or offers.
- `@astrojs/sitemap`.
- `robots.txt` allowing all.

**Accessibility**

- WCAG 2.2 AA contrast.
- Landmarks, one `h1`, logical heading order.
- Skip link.
- Visible `:focus-visible` rings.
- Keyboard-operable nav and accordion.
- Descriptive alt text for the hero.
- `prefers-reduced-motion` support.

**Performance**

- LCP under 2.0s on a simulated 4G connection.
- CLS under 0.05.
- Lighthouse 95+ for performance, accessibility, best practices, and SEO.
- No third-party requests.
- Hero image preloaded; everything below the fold lazy-loaded.
- No client-side framework hydration.

**Deployment**

- `npm run build` produces `dist/`.
- The deliverable ends at a production build plus deployment instructions
  unless the owner gives host and DNS access. Live deployment and DNS cutover
  are a separate step.
- Deploy to Cloudflare Pages, Netlify, or any static host; include a short
  deployment section in the website README.
- No redirects required beyond the optional `www` → apex redirect handled at
  the host.
- Cache immutable assets aggressively; HTML short-lived.

## 11. Claim ledger

| Claim | Source | Page wording |
| --- | --- | --- |
| Independent, not affiliated with Progress | `README.md` lines 5, 256–258 | Footer and FAQ |
| AGPL-3.0-only | `Cargo.toml`, `README.md` line 276 | Hero badge, support, footer |
| Rust implementation | `Cargo.toml`, `README.md` line 1 | Hero, support |
| Drop-in replacement | `README.md` line 5 | Supporting line, compatibility |
| 3,284 extended Pedant examples passing | `README.md` line 238 | Proof strip, link |
| Strict stock `chef-client` converge with shims off | `README.md` line 26, `docs/chef-client-e2e-report.md` | Proof strip, link |
| Cinc Client 19 on Debian, Ubuntu, Fedora | `docker-e2e/README.md` lines 13–16, 126 | Cinc section; verify by running the stack, otherwise use “ships a stack” |
| SQLite default, PostgreSQL supported | `README.md` line 14 | Under the hood |
| S3/Garage and OpenSearch/Elasticsearch | `README.md` line 23 | Under the hood |
| `chef-server-ctl` alias | `README.md` line 17 | Compatibility, tooling |
| Migration tools, no `chef-server.rb` importer | `README.md` lines 18–19, `crates/microwave-migrate/README.md` | Migration |
| `from-api` cannot copy password hashes or admin flag | `crates/microwave-migrate/README.md` lines 687–710 | Migration, known gaps |
| Multi-replica HA not validated | `README.md` lines 213, 216 | Under the hood, FAQ |
| Commercial support available | Owner-provided; wording pending | Support |

## 12. Open items for the owner

These are the only answers that would materially change the build:

### Resolved

- **Quickstart:** no command on the page. Secondary CTA links to the
  repository's run instructions. Revisit only when a public image exists.
- **Contact:** `hello@microwave.io` via `mailto:`.
- **Deployment:** production build plus deployment instructions. No live DNS
  cutover in this work.

### Still open

1. **Operator identity for the imprint.** Legal name, address, and contact
   details. Pages are built with a config placeholder; launch is blocked until
   the details are supplied.
2. **GitHub URL.** Default is the existing repository
   `https://github.com/rmoriz/microwave-development`. A future product org URL
   can replace it.
3. **Support wording.** The page describes migration help, deployment review,
   and ongoing support generically. If the actual offer has named packages,
   those replace the generic cards; no prices are invented.
4. **Cinc claim.** May the page say “tested with Cinc Client 19” once the
   Compose stack is run during implementation?

## 13. Acceptance criteria

- `npm run build` completes without errors and produces a static `dist/`.
- The page is responsive at 375px, 768px, and 1440px with no horizontal
  overflow.
- All copy is approved and contains no pricing attacks or unverified claims.
- The footer contains the independence, trademark, and license notices, plus
  working privacy and imprint links with owner-supplied details.
- “Talk to us”, GitHub, docs, and “Run it from source” actions all work and
  land on pages a visitor can actually use today.
- Keyboard navigation and a screen-reader pass are clean.
- `prefers-reduced-motion` disables non-essential motion.
- Lighthouse scores 95+ in all four categories on the production build.
- Initial page weight is under 1MB; hero image under 300KB on the largest
  variant.
- No cookies, analytics, or third-party runtime requests.
- The website README documents install, build, preview, and deployment.

## 14. Out of scope

Docs site, blog, pricing page, CMS, contact form backend, customer portal,
analytics, A/B testing, i18n, customer logos, benchmarks, and live chat.
