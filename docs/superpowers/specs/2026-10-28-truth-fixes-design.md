# Truth Fixes: Claims Inventory and CI Guardrails — Design

## Intent

Make the repository's factual claims about itself true, and keep them true.
The README is the adoption surface, the website links to its Remaining Work
anchor, and operators make deployment decisions from both. Today those
surfaces drift: the HA compose file says PostgreSQL is not available, the
README and TODO treat CI as existing, and the TODO status block lags the code
by days. A reader cannot tell a kept promise from a stale one.

The fix is not another prose pass. It is one machine-readable inventory of
every factual claim and every known gap, a generated Remaining Work document,
and CI that runs each claim's verification on every change and fails when a
claim, a manual recheck, or the rendered document goes stale.

## Why now

The recent audit found concrete false statements on live surfaces, and the
feature work already merged (PostgreSQL, MCP, run reports) is under-described
or mis-described. Every later Remaining Work phase will otherwise edit prose
that has no guardrail.

## Goals

- One source of truth for claims: `docs/claims.toml`.
- Generated `docs/remaining-work.md`; README keeps the anchor and shows a
  generated summary block with counts and per-area links.
- Every false or stale claim corrected across `README.md`, `TODO.md`, all of
  `docs/`, `docker-compose*.yml`, the example config, and `website/`.
- A claims checker that validates the inventory and runs cheap verifications.
- GitHub Actions: PR gate, PostgreSQL gate, nightly extended pedant gate.
- The pedant `headers` artifact fixed at the harness, so `strict` is green
  without a permanent allowlist.

## Non-goals

- Implementing any feature gap (LDAP, FIPS, Tantivy, telemetry, mail outbox,
  migration extensions).
- Making `docker-compose.ha.yml` a real multi-replica topology; only its
  description is corrected.
- Rewriting the dated reports; they get an as-of banner and a corrected
  headline status, and stay historical.
- Redesigning the website.
- Auto-filing GitHub issues from the inventory.

## Anti-goals

An inventory that copies prose verbatim and is never rechecked. A CI setup
that is red on its first run or needs repository secrets. A checker that
depends on network access for the PR gate. Claim entries that outlive the
code they describe.

## Constraints

- `AGENTS.md` test selection, website rules, and commit conventions apply.
- Website copy stays English, factual, and traceable; website changes run the
  homepage suite.
- Python 3 standard library and bash for tooling; no new Rust or JS
  dependencies. Ruby 3.4.12 for pedant.
- GitHub-hosted runners only; no secrets, forks must pass.

## Builder-check answers

- **`headers` over TLS does not work by itself.** The spec downgrades the same
  host and port to `http://`, while the server listener is either HTTP or TLS
  (`bin/microwave-server/src/main.rs`), and with an `http` base URL `sub!`
  returns `nil`. Fix at the harness with a one-line local patch to
  `chef-server/oc-chef-pedant/spec/api/header_spec.rb` (`request_url` instead
  of `request_url.sub!("https", "http")`), and record the patch in
  `scripts/run-pedant.sh` and the reports. No second listener, proxy, or TLS
  mode; `strict` runs over plain HTTP.
- **Vendored patches are allowed** when they are harness-only and documented
  next to the harness. A future re-vendor must reapply them; the script header
  carries the list.
- **The HA compose file stays functionally as it is.** Correct the prose to
  say SQLite metadata on the local volume, shared OpenSearch and Garage, one
  replica, and the experimental PostgreSQL switch. The same stale sentence in
  `docker-compose.yml` is corrected too.
- **Scope is every claim in `README.md`, `TODO.md`, all of `docs/`, and
  `website/`**, not only the gap list. Dated reports are inventoried by their
  headline status and baseline numbers, which are `manual` claims tied to a
  report; the bodies stay historical under a banner.
- **README keeps a generated block**, not hand-written counts: the renderer
  owns a marked `<!-- generated:remaining-work -->` region containing counts
  and per-area links into `docs/remaining-work.md`, and `--check` covers both
  files. The pedant numbers stay as `manual` claims pointing at the reports.
- **`TODO.md` gets a full sweep**, not just the snapshot: each unchecked box is
  verified and ticked when implemented, open ones gain a claim id, and the
  phase trace is kept.
- **`scripts/run-postgres-tests.sh` honours a supplied
  `MICROWAVE_TEST_POSTGRES_URL`** and only provisions its own container when
  the variable is unset. The workflow uses a service container and installs
  `postgresql-client`; it runs on pull requests and `main`.
- **Website gaps link to claim ids**: each KnownGaps item links to
  `docs/remaining-work.md#<id>`, and the content test asserts every linked id
  exists in `claims.toml` text.

## Design
## Approaches considered

1. **Inventory first.** Correct the false claims, then add a checked
   inventory, renderer, and CI. Durable and auditable; largest initial diff.
2. **CI first.** Add workflows and fix what breaks. Fast, but coverage is
   whatever the checks happen to touch and the inventory never exists.
3. **Patch only.** Fix known lines and add basic CI. Cheapest, re-breaks on
   the next docs change.

Chosen: 1, with the correction commits landing before the checker so every
checker run is green.

Calls made here rather than by the operator: the checker is Python;
`remaining-work.md` is generated; harness-only vendored patches are allowed.

## Design

### 1. Inventory format

`docs/claims.toml` holds one `[[claim]]` table per claim:

```toml
[[claim]]
id = "ha-compose-postgres"
summary = "The HA compose override uses PostgreSQL, OpenSearch and Garage."
area = "deployment"
status = "verified"            # verified | gap | manual
surfaces = [
  { path = "docker-compose.ha.yml", needle = "PostgreSQL" },
]
verify = { kind = "grep", path = "docker-compose.ha.yml", pattern = "database.driver" }
```

Fields: unique `id`, one-line `summary`, `area` (deployment, migration, mail,
search, ha, auth, tooling, docs), `status`, one or more `surfaces` with an
optional literal `needle`, and a `verify` entry. Verify kinds: `file`,
`grep`, `cargo` (integration test path plus test name), `npm` (test file),
`manual` (dated recheck, 90-day expiry). `status = "gap"` entries render into
`remaining-work.md`; `verified` and `manual` entries do not.

### 2. Renderer

`scripts/render-remaining-work.py` writes `docs/remaining-work.md` from gap
claims grouped by area, each with its id and the surfaces it affects, and
rewrites the marked README block with counts and per-area links. `--check`
renders both to buffers and exits non-zero on a diff. The generated file opens
with a note that it is generated and where to edit.

### 3. Checker

`scripts/check-claims.py` runs in the PR gate and checks: TOML parses, ids
unique, areas/statuses/kinds valid, every surface path exists, every needle is
present, every `file`/`grep`/`cargo`/`npm` target resolves, every `manual`
entry is within 90 days, every README and website claim link points at an
existing id, and both rendered outputs are current. It runs the `file` and
`grep` verifications directly and resolves the rest; the matching `cargo` and
`npm` suites run in CI anyway.

### 4. README

Replace the ten bullets with the generated summary block, keeping the
`## Remaining Work` heading so `website/homepage` anchors keep working. The
block links the areas, not the individual claims; individual claims live in
`docs/remaining-work.md`.

### 5. Corrections

Sweep every surface and correct what is false, stale, or unverifiable. Known
targets:

- `docker-compose.ha.yml` and `docker-compose.yml`: backend status and
  single-replica wording.
- `README.md`: CI wording, MCP precision, pedant artifact, counts.
- `TODO.md`: status snapshot, verified boxes, ids on open boxes.
- `docs/`: deployment backend/HA text, dependency-audit CI section, pedant
  reports (banner plus current status), MCP guide and every other guide's
  factual statements.
- `website/homepage`: KnownGaps, FAQ, and any other copy the sweep finds; each
  gap links its claim id; content tests assert the links.

### 6. CI

- `.github/workflows/ci.yml` on PR and push to `main`: `cargo fmt --check`,
  `cargo clippy -D warnings`, `cargo test --workspace`,
  `scripts/check-claims.py`, and `website/homepage` `npm test`.
- `.github/workflows/postgres.yml` on PR and `main`: PostgreSQL service
  container and client tools, then
  `MICROWAVE_REQUIRE_POSTGRES=1 scripts/run-postgres-tests.sh`, which uses the
  supplied URL instead of starting its own container.
- `.github/workflows/pedant.yml`: nightly and `workflow_dispatch`; Ruby
  3.4.12, build, `scripts/run-pedant.sh strict`, artifacts on failure.

### 7. Pedant header patch

Patch `chef-server/oc-chef-pedant/spec/api/header_spec.rb` to request
`request_url` instead of `request_url.sub!("https", "http")`. List the patch in
the `scripts/run-pedant.sh` header and in both reports. `headers` then passes
over plain HTTP and `strict` is green without an allowlist.

## Files

- Create: `docs/claims.toml`, `docs/remaining-work.md`,
  `scripts/check-claims.py`, `scripts/render-remaining-work.py`,
  `.github/workflows/ci.yml`, `.github/workflows/postgres.yml`,
  `.github/workflows/pedant.yml`.
- Modify: `README.md`, `TODO.md`, `docker-compose.yml`,
  `docker-compose.ha.yml`, `packaging/microwave.toml.example`, `AGENTS.md`,
  `docs/**`, `website/homepage/**`, `scripts/run-pedant.sh`,
  `scripts/run-postgres-tests.sh`,
  `chef-server/oc-chef-pedant/spec/api/header_spec.rb`.

## Rollout order

1. Claim corrections across docs, TODO, README, and website, each with its
   suite green.
2. Pedant header patch and harness note; `strict` green.
3. Inventory plus renderer; render `remaining-work.md` and the README block.
4. Checker and link wiring in README, TODO, and website.
5. CI workflows and the PostgreSQL URL override.

## Review focus

1. A missing or renamed claim id leaves a dead README or website anchor.
2. A manual claim passes forever because its date never expires.
3. Forks without the vendored tree or Ruby fail the required gate.
4. The rendered README block is edited by hand and the check is skipped.
5. The TODO sweep ticks a box the tests do not actually cover.

## Left to the builder

Exact correction wording; area grouping and counts; inventory granularity per
document; workflow runner images, timeouts, and artifact retention; how the
checker reports the first stale item without dumping the whole TOML.
