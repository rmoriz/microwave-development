# Truth Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every factual claim in the README, TODO, `docs/`, the compose files, and the website true, and keep it true with a checked claim inventory and CI.

**Architecture:** A single TOML inventory (`docs/claims.toml`) is the source of truth. A renderer produces `docs/remaining-work.md` and a marked README summary block; a checker validates the inventory, each claim's surface and verification target, manual-check expiry, generated output, and every inbound claim link. CI runs the checker, the Rust and website suites, a PostgreSQL matrix, and a nightly pedant run. Corrections land surface by surface before the workflows are enabled.

**Tech Stack:** Python 3 standard library (`tomllib`, `unittest`, `re`), bash, GitHub Actions, existing Rust workspace, existing Astro/Playwright homepage, vendored `oc-chef-pedant` with Ruby 3.4.12.

**Spec:** `docs/superpowers/specs/2026-10-28-truth-fixes-design.md`

## Global Constraints

- `AGENTS.md` test selection, website rules, and commit conventions apply.
- Website copy is English, playful but factual, traceable to the repository or `docs/`, and never says `AI-powered`/`built-in AI`; MCP is always `experimental`, writes are on by default and switched off with `writes = false`.
- The website must work with JavaScript disabled, avoid horizontal overflow at 320/375/768/900/1440 px, keep one `h1` and a sane heading order, and add no runtime request, framework, or dependency.
- `npm test` runs `astro check`, content tests, and Playwright with axe. Website or `docs/mcp.md` changes run it.
- Rust changes (only the pedant spec patch here) run `cargo fmt --all --check` and `cargo clippy --workspace --all-targets`.
- Python 3 standard library only; Ruby 3.4.12 for pedant; no new Rust or JS dependencies.
- GitHub-hosted runners only; no repository secrets; fork PRs must pass the required checks.
- Claim ids match `^[a-z0-9]+(-[a-z0-9]+)*$` and are unique.
- Areas are exactly: `deployment`, `migration`, `mail`, `search`, `ha`, `auth`, `tooling`, `docs`, `website`.
- Statuses are exactly: `verified`, `gap`, `manual`. Verify kinds are exactly: `file`, `grep`, `cargo`, `npm`, `manual`.
- A `manual` claim expires 90 days after its `checked` date.
- Every file under `README.md`, `TODO.md`, `docs/*.md` (excluding `docs/claims.toml` and generated `docs/remaining-work.md`), and `website/homepage/src/{pages,components,data}/**` must be named by at least one claim surface. `docs/superpowers/**` is exempt.
- Generated regions are `docs/remaining-work.md` and the block between `<!-- generated:remaining-work:start -->` and `<!-- generated:remaining-work:end -->` in `README.md`. Only the renderer writes them.
- README and website links to claims use the form `remaining-work.md#<id>` (website prefixes the GitHub blob URL).

## Review Focus

1. A renamed or deleted claim id leaves a dead anchor on the README or website — the link check in Task 3 must cover both surfaces.
2. A `manual` claim never expires and quietly blesses stale prose — Task 3 must test a 91-day-old entry.
3. Hand editing either generated region drifts it from the inventory — Task 1 and Task 3 must test the `--check` diff for both files.
4. Fork PRs and runners without Ruby must still pass the required gate — Task 2 keeps pedant on `schedule`/`workflow_dispatch` only and the PR workflows install no Ruby.
5. A TODO box is ticked although no test covers it — every ticked box in Tasks 7–9 must name the command that proves it.

## File Structure

- `scripts/claims_lib.py` — schema loading, validation, rendering, block replacement. Imported by both scripts and tests.
- `scripts/render-remaining-work.py` — CLI wrapper, `--check`.
- `scripts/check-claims.py` — CLI wrapper, `--check` semantics, link and coverage checks.
- `scripts/tests/test_claims_lib.py` — unit tests with temporary roots.
- `docs/claims.toml` — the inventory.
- `docs/remaining-work.md` — generated gap list.
- `.github/workflows/{ci,postgres,pedant}.yml` — gates.
- Surface files audited and corrected in Tasks 4–13.

---

### Task 1: Claim library and renderer

**Files:**
- Create: `scripts/claims_lib.py`, `scripts/render-remaining-work.py`, `scripts/tests/test_claims_lib.py`

**Interfaces:**
- Consumes: nothing.
- Produces: `Claim` dataclass with `id, summary, area, status, surfaces, verify`; `Surface(path, needle)`; `Verify(kind, path, pattern, test, checked, ref)`; `load_claims(path: Path) -> list[Claim]`; `render_doc(claims: list[Claim]) -> str`; `render_block(claims: list[Claim]) -> str`; `replace_block(text: str, block: str) -> str`; `DOC_START`, `DOC_END`, `BLOCK_START = "<!-- generated:remaining-work:start -->"`, `BLOCK_END = "<!-- generated:remaining-work:end -->"`.

- [ ] **Step 1: Write the failing renderer tests**

```python
# scripts/tests/test_claims_lib.py
import tempfile, unittest
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import claims_lib

GAP = claims_lib.Claim(
    id="compose-postgres",
    summary="The HA compose override names PostgreSQL.",
    area="deployment",
    status="gap",
    surfaces=[claims_lib.Surface("docker-compose.ha.yml", "PostgreSQL")],
    verify=claims_lib.Verify(kind="grep", path="docker-compose.ha.yml", pattern="database.driver"),
)

class RenderTest(unittest.TestCase):
    def test_doc_groups_by_area_and_anchors_ids(self):
        other = claims_lib.Claim("mail-outbox", "No durable outbox.", "mail", "gap",
                                 [claims_lib.Surface("docs/deployment.md", "outbox")],
                                 claims_lib.Verify(kind="grep", path="docs/deployment.md", pattern="outbox"))
        text = claims_lib.render_doc([other, GAP])
        self.assertLess(text.index('<a id="area-deployment">'), text.index('<a id="area-mail">'))
        self.assertIn('<a id="compose-postgres">', text)
        self.assertIn("docker-compose.ha.yml", text)
        self.assertIn("Open gaps: 2", text)

    def test_doc_omits_verified_claims(self):
        verified = claims_lib.Claim("cli-exists", "CLI exists.", "tooling", "verified",
                                    [claims_lib.Surface("README.md", "microwave-ctl")],
                                    claims_lib.Verify(kind="file", path="bin/microwave-ctl"))
        text = claims_lib.render_doc([verified])
        self.assertIn("Open gaps: 0", text)
        self.assertNotIn('<a id="cli-exists">', text)

    def test_block_counts_and_area_links(self):
        block = claims_lib.render_block([GAP])
        self.assertTrue(block.startswith(claims_lib.BLOCK_START))
        self.assertTrue(block.endswith(claims_lib.BLOCK_END))
        self.assertIn("Open gaps: 1", block)
        self.assertIn("remaining-work.md#area-deployment", block)

    def test_replace_block_keeps_surroundings(self):
        readme = "before\n" + claims_lib.BLOCK_START + "\nstale\n" + claims_lib.BLOCK_END + "\nafter\n"
        replaced = claims_lib.replace_block(readme, "NEW")
        self.assertIn("NEW", replaced)
        self.assertNotIn("stale", replaced)
        self.assertTrue(replaced.startswith("before\n"))
        self.assertTrue(replaced.endswith("\nafter\n"))

    def test_replace_block_requires_markers(self):
        with self.assertRaises(ValueError):
            claims_lib.replace_block("no markers\n", "NEW")
```

- [ ] **Step 2: Run the tests and see the import fail**

Run: `python3 -m unittest discover -s scripts/tests -p 'test_claims_lib.py' -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'claims_lib'`.

- [ ] **Step 3: Implement the library**

Dataclasses with `slots=True`, `load_claims` using `tomllib` and raising `ValueError` for unknown fields and wrong types. `render_doc` order is the fixed area order from Global Constraints, then claim id; it writes the generated banner, `Open gaps: <n>` with per-area counts, then per-area `<a id="area-<area>"></a>` headings and per-gap `<a id="<id>"></a>` blocks containing the summary, the id, and each surface as `` `path` ``. `render_block` returns the marked one-paragraph summary with `remaining-work.md#area-<area>` links. `replace_block` replaces only the text between `BLOCK_START`/`BLOCK_END` and raises `ValueError` when either marker is missing or duplicated.

`scripts/render-remaining-work.py` reads `docs/claims.toml`, writes `docs/remaining-work.md`, rewrites `README.md`, and with `--check` compares both rendered results to disk and exits 1 on any difference.

- [ ] **Step 4: Run the tests and the renderer**

Run: `python3 -m unittest discover -s scripts/tests -p 'test_claims_lib.py' -v && python3 scripts/render-remaining-work.py --check`
Expected: 5 tests PASS. The renderer exits 1 because no inventory or generated files exist yet; that is expected until Task 4.

- [ ] **Step 5: Commit**

```bash
git add scripts/claims_lib.py scripts/render-remaining-work.py scripts/tests/test_claims_lib.py
git commit -m "feat(claims): add inventory renderer"
```

---

### Task 2: PR workflows and PostgreSQL URL override

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/postgres.yml`, `.github/workflows/pedant.yml`
- Modify: `scripts/run-postgres-tests.sh:70-105`
- Test: workflow lint via `act`-free YAML parse and a local script run

**Interfaces:**
- Consumes: `scripts/render-remaining-work.py` from Task 1 (invoked once claims exist; the step may fail until Task 13).
- Produces: check names `fmt`, `clippy`, `test`, `claims`, `homepage`, `postgres`, `pedant`.

- [ ] **Step 1: Write the failing URL-override test**

Add to `scripts/run-postgres-tests.sh` a guard the workflow depends on: when `MICROWAVE_TEST_POSTGRES_URL` is already set, the script must not call `docker`. Prove it first:

Run: `MICROWAVE_TEST_POSTGRES_URL=postgres://nobody@127.0.0.1:1/none MW_SKIP_DOCKER=1 scripts/run-postgres-tests.sh --check-url`
Expected: FAIL with `unknown argument --check-url` (or the script starts a container).

- [ ] **Step 2: Implement the override and `--check-url`**

In `scripts/run-postgres-tests.sh`, after argument parsing: if `MICROWAVE_TEST_POSTGRES_URL` is non-empty, skip `docker run`/`docker rm`, export the value unchanged, and print `using supplied MICROWAVE_TEST_POSTGRES_URL`. `--check-url` prints the resolved URL with the password redacted and exits 0 without connecting. Keep the self-provisioning path for an unset variable and the existing trap-based cleanup.

- [ ] **Step 3: Run the guard test**

Run: `MICROWAVE_TEST_POSTGRES_URL=postgres://nobody@127.0.0.1:1/none scripts/run-postgres-tests.sh --check-url`
Expected: PASS, prints `postgres://nobody@127.0.0.1:1/none`, no `docker` call.

- [ ] **Step 4: Write the three workflows**

`ci.yml`: triggers `pull_request` and `push` to `main`; `ubuntu-24.04`; steps `checkout`, `dtolnay/rust-toolchain@stable` with `components: rustfmt, clippy`, `actions/setup-python@v5` with `python-version: "3.13"`, `actions/setup-node@v4` with `node-version: "22"` and npm cache on `website/homepage/package-lock.json`; jobs `rust` (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`), `claims` (`python3 scripts/check-claims.py`), `homepage` (`npm ci && npm test` in `website/homepage`). Cache `target` with `Swatinem/rust-cache@v2`.

`postgres.yml`: triggers `pull_request` and `push` to `main`; service `postgres:18` with database `microwave_test`, user `microwave`, password `microwave`, health check `pg_isready`; install `postgresql-client-18`; run `MICROWAVE_REQUIRE_POSTGRES=1 MICROWAVE_TEST_POSTGRES_URL=postgres://microwave:microwave@localhost:5432/microwave_test scripts/run-postgres-tests.sh`.

`pedant.yml`: triggers `schedule` (`17 3 * * *`) and `workflow_dispatch` only; `ubuntu-24.04`; `ruby/setup-ruby@v1` with `ruby-version: "3.4.12"`; `cargo build -p microwave-server`; `scripts/run-pedant.sh strict`; `actions/upload-artifact@v4` on failure with `if-no-files-found: ignore`.

- [ ] **Step 5: Validate YAML and commit**

Run: `python3 -c "import sys,yaml;[yaml.safe_load(open(p)) for p in sys.argv[1:]]" .github/workflows/ci.yml .github/workflows/postgres.yml .github/workflows/pedant.yml`
Expected: exit 0 (if PyYAML is absent locally, use `ruby -ryaml -e 'ARGV.each { |f| YAML.load_file(f) }' .github/workflows/*.yml`).

```bash
git add .github/workflows scripts/run-postgres-tests.sh
git commit -m "chore(ci): add PR, PostgreSQL, and nightly pedant gates"
```

---

### Task 3: Claim checker

**Files:**
- Create: `scripts/check-claims.py`
- Modify: `scripts/tests/test_claims_lib.py`

**Interfaces:**
- Consumes: Task 1 library and the markers.
- Produces: `validate(claims, root: Path, today: date) -> list[str]` returning `"<path>:<claim-id>: <message>"` strings; `coverage_gaps(claims, root) -> list[str]`; `link_errors(claims, root) -> list[str]`; CLI `--no-coverage` skips the coverage rule for partially inventoried work, and the default `--check` mode exits 1 when any list is non-empty.

- [ ] **Step 1: Write the failing tests**

```python
from datetime import date

class ValidateTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        (self.root / "docs").mkdir()
        (self.root / "docs" / "a.md").write_text("PostgreSQL is available.\n")

    def tearDown(self):
        self.tmp.cleanup()

    def claim(self, **over):
        base = dict(id="x", summary="s", area="docs", status="verified",
                    surfaces=[claims_lib.Surface("docs/a.md", "PostgreSQL")],
                    verify=claims_lib.Verify(kind="grep", path="docs/a.md", pattern="PostgreSQL"))
        base.update(over)
        return claims_lib.Claim(**base)

    def test_valid_claim_passes(self):
        self.assertEqual(claims_lib.validate([self.claim()], self.root, date(2026, 10, 28)), [])

    def test_missing_needle_is_reported(self):
        claim = self.claim(surfaces=[claims_lib.Surface("docs/a.md", "SQLite")])
        self.assertIn("docs/a.md:x:", claims_lib.validate([claim], self.root, date(2026, 10, 28))[0])

    def test_duplicate_id_is_reported(self):
        errors = claims_lib.validate([self.claim(), self.claim()], self.root, date(2026, 10, 28))
        self.assertTrue(any("duplicate" in e for e in errors))

    def test_path_escape_is_reported(self):
        claim = self.claim(verify=claims_lib.Verify(kind="file", path="../outside"))
        self.assertTrue(any("outside" in e for e in claims_lib.validate([claim], self.root, date(2026, 10, 28))))

    def test_manual_claim_expires_after_ninety_days(self):
        claim = self.claim(status="manual",
                           verify=claims_lib.Verify(kind="manual", checked=date(2026, 7, 29), ref="docs/a.md"))
        errors = claims_lib.validate([claim], self.root, date(2026, 10, 28))
        self.assertTrue(any("expired" in e for e in errors))

    def test_fresh_manual_claim_passes(self):
        claim = self.claim(status="manual",
                           verify=claims_lib.Verify(kind="manual", checked=date(2026, 10, 28), ref="docs/a.md"))
        self.assertEqual(claims_lib.validate([claim], self.root, date(2026, 10, 28)), [])

    def test_missing_verify_target_is_reported(self):
        claim = self.claim(verify=claims_lib.Verify(kind="file", path="docs/missing.md"))
        self.assertTrue(any("missing.md" in e for e in claims_lib.validate([claim], self.root, date(2026, 10, 28))))

    def test_uncovered_doc_is_reported(self):
        (self.root / "docs" / "b.md").write_text("x\n")
        self.assertIn("docs/b.md", claims_lib.coverage_gaps([self.claim()], self.root))

    def test_link_to_unknown_id_is_reported(self):
        (self.root / "README.md").write_text("see remaining-work.md#nope\n")
        self.assertIn("nope", claims_lib.link_errors([self.claim()], self.root)[0])

    def test_render_check_detects_hand_edit(self):
        (self.root / "docs" / "remaining-work.md").write_text("stale\n")
        self.assertTrue(claims_lib.render_errors([self.claim()], self.root))
```

- [ ] **Step 2: Run and watch it fail**

Run: `python3 -m unittest discover -s scripts/tests -p 'test_claims_lib.py' -v`
Expected: FAIL with `AttributeError: module 'claims_lib' has no attribute 'validate'`.

- [ ] **Step 3: Implement validation**

`validate` checks types and enums, id regex and uniqueness, surface paths (relative, inside root, existing file, non-empty needle present), verify targets per kind (`file` exists; `grep` pattern compiles and matches; `cargo` test file exists and the `::` suffix occurs in it; `npm` file exists; `manual` date parses, `(today - checked).days <= 90`, `ref` exists), and that a `gap` has at least one surface. `coverage_gaps` applies the Global Constraints path list, skipping `docs/superpowers/**`, `docs/claims.toml`, and `docs/remaining-work.md`. `link_errors` scans `README.md` and `website/homepage/src/**` for `remaining-work\.md#([a-z0-9-]+)` and reports unknown ids. `render_errors` uses Task 1's renderer against `docs/remaining-work.md` and the README block.

`scripts/check-claims.py` loads `docs/claims.toml`, applies the checks according to the flags, prints every error one per line, and exits 1 when non-empty.

- [ ] **Step 4: Run tests**

Run: `python3 -m unittest discover -s scripts/tests -p 'test_claims_lib.py' -v`
Expected: 15 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add scripts/check-claims.py scripts/claims_lib.py scripts/tests/test_claims_lib.py
git commit -m "feat(claims): validate inventory, links, and generated output"
```

---

### Task 4: Seed inventory, compose, packaging, and first corrections

**Files:**
- Create: `docs/claims.toml`, `docs/remaining-work.md`
- Modify: `README.md` (add markers around the section body), `docker-compose.ha.yml:1-20,95-110`, `docker-compose.yml:10-18`, `packaging/microwave.toml.example`

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: `version = 1` inventory with the five claims below; generated files exist.

- [ ] **Step 1: Create the inventory with these claims**

Add `version = 1` and these entries. Add one `verified` `file` claim for each surface file so the inventory owns them from the start; coverage across the rest of the repo is completed by Tasks 5–10.

```toml
[[claim]]
id = "ha-compose-sqlite-metadata"
summary = "The HA compose override keeps SQLite metadata and shares only search and blobs."
area = "ha"
status = "gap"
surfaces = [
  { path = "docker-compose.ha.yml", needle = "keeps the metadata store on the local SQLite volume" },
  { path = "docker-compose.ha.yml", needle = "PostgreSQL" },
]
verify = { kind = "grep", path = "docker-compose.ha.yml", pattern = "keeps the metadata store on the local SQLite" }

[[claim]]
id = "ha-compose-single-replica"
summary = "The HA compose file is a single-server evaluation topology, not multi-replica HA."
area = "ha"
status = "verified"
surfaces = [ { path = "docker-compose.ha.yml", needle = "not a multi-replica HA deployment" } ]
verify = { kind = "grep", path = "docker-compose.ha.yml", pattern = "not a multi-replica HA deployment" }

[[claim]]
id = "compose-postgres-available"
summary = "PostgreSQL is a selectable metadata backend."
area = "deployment"
status = "gap"
surfaces = [ { path = "docker-compose.yml", needle = "PostgreSQL metadata is" } ]
verify = { kind = "grep", path = "docker-compose.yml", pattern = "database.driver" }

[[claim]]
id = "packaging-example-complete"
summary = "The example configuration documents every supported block without stale disabled notes."
area = "deployment"
status = "verified"
surfaces = [ { path = "packaging/microwave.toml.example", needle = "telemetry" } ]
verify = { kind = "grep", path = "packaging/microwave.toml.example", pattern = "no outbound sender is implemented" }

[[claim]]
id = "readme-remaining-work-generated"
summary = "The README Remaining Work section is generated from docs/claims.toml."
area = "docs"
status = "gap"
surfaces = [ { path = "README.md", needle = "generated:remaining-work:start" } ]
verify = { kind = "grep", path = "README.md", pattern = "generated:remaining-work:start" }
```

- [ ] **Step 2: Run the checker and watch it fail**

Run: `python3 scripts/check-claims.py --no-coverage --no-coverage`
Expected: FAIL on the two missing compose needles, the missing README markers, and the missing generated doc. This is the worklist for the next steps.

- [ ] **Step 3: Correct the compose and example text**

`docker-compose.ha.yml`: replace the header's "SQLite is the only implemented metadata backend; PostgreSQL is not available yet" with the truth: the override keeps SQLite metadata on the local volume, the server supports PostgreSQL via `MW_DATABASE__DRIVER=postgres` plus `MW_DATABASE__URL`, and this file stays single-replica with one writer. Replace the line-103 comment the same way. In `docker-compose.yml`, correct the "PostgreSQL metadata is not available yet" sentence to point at `database.driver`/`MW_DATABASE__DRIVER`. In `packaging/microwave.toml.example`, make every block comment match the implemented default (especially MCP `writes`, telemetry, mail).

- [ ] **Step 4: Add the README markers and render**

Wrap the entire `## Remaining Work` bullet list in the two markers, then run `python3 scripts/render-remaining-work.py`. Confirm `docs/remaining-work.md` exists and the README block shows `Open gaps: 3` (or the current count).

- [ ] **Step 5: Run the checker, tests, and commit**

Run: `python3 scripts/check-claims.py --no-coverage --no-coverage; python3 -m unittest discover -s scripts/tests -v`
Expected: PASS for both.

```bash
git add docs/claims.toml docs/remaining-work.md README.md docker-compose.yml docker-compose.ha.yml packaging/microwave.toml.example
git commit -m "docs(claims): seed inventory and correct compose backend claims"
```

---

### Task 5: README audit and corrections

**Files:**
- Modify: `README.md`
- Modify: `docs/claims.toml`

- [ ] **Step 1: Correct the known false statements**

- Line 19: say the endpoint is opt-in/disabled and that the eight write tools appear only with `writes = true`.
- Line 216: point at the workflow names from Task 2 instead of the vague "in CI".
- The `headers` artifact sentences (lines 216, 240, 242) become: fixed at the harness; see `docs/pedant-report.md`.
- Status snapshot wording: keep only claims the suites prove; the layout row for `.github/` is added.
- Add a `.github/workflows/` row to the Layout table.

- [ ] **Step 2: Inventory every remaining factual README sentence**

Add one claim per factual sentence across Status, Run Locally, TLS, Mail, Dual-backend store matrix, HTTP route parity, Deployment, and Operational CLI Notes. Use `file`/`grep`/`cargo`/`npm` verifies; use `manual` with `ref = "docs/pedant-report.md"` for the pedant numbers. Known stale targets get `gap` with the aspirational verify target, then a correction. Do not inventory headings or pure commands.

- [ ] **Step 3: Run the checker and fix what it reports**

Run: `python3 scripts/check-claims.py --no-coverage`
Expected: PASS. Any `gap` left in README must be a real open item that also exists in `docs/remaining-work.md`.

- [ ] **Step 4: Re-render and commit**

Run: `python3 scripts/render-remaining-work.py && python3 scripts/check-claims.py --no-coverage`
Expected: PASS.

```bash
git add README.md docs/claims.toml docs/remaining-work.md
git commit -m "docs(readme): correct CI, MCP, and pedant claims"
```

---

### Task 6: Website audit and claim links

**Files:**
- Modify: `website/homepage/src/components/KnownGaps.astro`, `website/homepage/src/components/FAQ.astro`, other `src/{pages,components,data}` files the audit finds, `website/homepage/tests/content/site.test.mjs`, `docs/claims.toml`
- Test: `website/homepage/tests/content/site.test.mjs`

- [ ] **Step 1: Write the failing link test**

Add to `site.test.mjs` a test that reads `docs/claims.toml`, extracts every `href` containing `remaining-work.md#`, and asserts each fragment exists as `id = "<fragment>"` in the TOML, and that every `KnownGaps` entry has one.

```js
test('known gaps link to catalogued claims', () => {
  const claims = fs.readFileSync(path.resolve(root, '../../docs/claims.toml'), 'utf8');
  const ids = $('.gaps-list a')
    .map((_, a) => $(a).attr('href').split('#')[1])
    .get();
  assert.ok(ids.length >= 7, 'every known gap carries a claim link');
  for (const id of ids) assert.match(claims, new RegExp(`id = "${id}"`));
});
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd website/homepage && npm test`
Expected: FAIL — no `remaining-work.md` links are rendered.

- [ ] **Step 3: Correct the copy and wire the links**

Turn the `KnownGaps` array into `{ text, claim }` objects, render `<a href={`${site.github}/blob/main/docs/remaining-work.md#${claim}`}>` around each. Audit FAQ, `UnderTheHood`, `Hero`, `Compatibility`, `FinalCTA`, `site.ts`, and every other copy file against the code and inventory; correct the stale sentences (e.g. the SQLite-only and CI wording) and add a claim each to `docs/claims.toml`. Keep the prose rules from Global Constraints.

- [ ] **Step 4: Add inventory entries for all corrected and existing copy**

Each website file must be covered. Use `grep` verifies against the `.astro`/`.ts` source, not built HTML.

- [ ] **Step 5: Run the suite, checker, and commit**

Run: `cd website/homepage && npm test && cd ../.. && python3 scripts/check-claims.py --no-coverage && python3 scripts/render-remaining-work.py`
Expected: PASS.

```bash
git add website/homepage docs/claims.toml docs/remaining-work.md
git commit -m "docs(website): correct gaps and link them to claims"
```

---

### Task 7: TODO status snapshot and Phases 1–2

**Files:**
- Modify: `TODO.md`, `docs/claims.toml`

- [ ] **Step 1: Rewrite the status snapshot**

Replace the 2026-10-25 block with today's date and three sections: implemented (pointer to tests), verified (commands), open (claim ids). Keep the "design trace" note.

- [ ] **Step 2: Phase 1 and 2 box sweep**

For each unchecked box in Phases 1 and 2, verify with a concrete command before deciding:

- config/crypto/store/authz: `grep` the symbol or test (`rg -n "<symbol>" crates/`), or run the named test (`cargo test -p <crate> <name>`);
- HTTP routes: `rg -n "<route>" crates/microwave-api/src`;
- behavior: the matching integration test.

Tick the box and append `<!-- verified: <command> -->`. If it is open, append `<!-- claim:<id> -->` and add the claim with a `gap` status. Fix the surviving false sentences in the same file.

- [ ] **Step 3: Run the checker and commit**

Run: `python3 scripts/check-claims.py --no-coverage && python3 scripts/render-remaining-work.py`
Expected: PASS.

```bash
git add TODO.md docs/claims.toml docs/remaining-work.md
git commit -m "docs(todo): verify phase 1-2 status and catalog gaps"
```

---

### Task 8: TODO Phases 3–4

**Files:**
- Modify: `TODO.md`, `docs/claims.toml`

- [ ] **Step 1: Phase 3 and 4 sweep**

Same rule as Task 7 for nodes, roles, environments, data bags, bookshelf, cookbooks, artifacts, policies, universe, depsolver, search expansion, parser, and backends. Tick with a verification comment, or link a claim id.

- [ ] **Step 2: Run the checker and commit**

Run: `python3 scripts/check-claims.py --no-coverage && python3 scripts/render-remaining-work.py`
Expected: PASS.

```bash
git add TODO.md docs/claims.toml docs/remaining-work.md
git commit -m "docs(todo): verify phase 3-4 status and catalog gaps"
```

---

### Task 9: TODO Phase 5 and final checklist

**Files:**
- Modify: `TODO.md`, `docs/claims.toml`

- [ ] **Step 1: Phase 5 sweep**

Same rule for OAuth, data collector, CLI, migrate, P1/P2/P3/P4, SMTP, and the final verification list. Resolve the duplicate `5.4`/`5.5` numbering. Add a checked `ci` box pointing at the workflows once Task 2 exists.

- [ ] **Step 2: Run the checker and commit**

Run: `python3 scripts/check-claims.py --no-coverage && python3 scripts/render-remaining-work.py`
Expected: PASS.

```bash
git add TODO.md docs/claims.toml docs/remaining-work.md
git commit -m "docs(todo): verify phase 5 status and catalog remaining gaps"
```

---

### Task 10: Deployment guide, MCP guide, and remaining docs

**Files:**
- Modify: `docs/deployment.md`, `docs/mcp.md`, `docs/debian-package.md`, `docs/dependency-audit.md`, and any other top-level guide the audit finds except the two pedant reports (Task 12); `docs/claims.toml`
- Test: `website/homepage` `npm test`

- [ ] **Step 1: Audit and correct**

For each top-level doc: read it against the code, correct false/stale statements (backend availability, HA wording, CI recommendations, MCP defaults, disabled/experimental status), and add claims for every factual sentence. Use `grep`/`file`/`cargo` verifies. The dependency-audit "recommended CI addition" becomes the real workflow names. Add cross-links from the guides' gap statements to claim ids.

- [ ] **Step 2: Run the guide tests, checker, and commit**

Run: `cd website/homepage && npm test && cd ../.. && python3 scripts/check-claims.py --no-coverage && python3 scripts/render-remaining-work.py`
Expected: PASS.

```bash
git add docs website/homepage docs/claims.toml docs/remaining-work.md
git commit -m "docs(guides): align deployment, MCP, and audit claims with the code"
```

---

### Task 11: Pedant header patch

**Files:**
- Modify: `chef-server/oc-chef-pedant/spec/api/header_spec.rb:45`, `scripts/run-pedant.sh:1-30`
- Test: `scripts/run-pedant.sh headers`

- [ ] **Step 1: Prove the current failure**

Run: `scripts/run-pedant.sh headers`
Expected: FAIL with `NoMethodError`/500 in the `nginx_default_error` example because `sub!` returns `nil`.

- [ ] **Step 2: Apply the one-line patch and document it**

Change `get(request_url.sub!("https", "http"), superuser)` to `get(request_url, superuser)`. Add a comment block at the top of `scripts/run-pedant.sh` listing harness-only vendored patches (currently just this one) and the re-vendor obligation.

- [ ] **Step 3: Re-run the tag**

Run: `scripts/run-pedant.sh headers`
Expected: `7 examples, 0 failures`.

- [ ] **Step 4: Commit**

```bash
git add chef-server/oc-chef-pedant/spec/api/header_spec.rb scripts/run-pedant.sh
git commit -m "test(pedant): fix the header URL artifact at the harness"
```

---

### Task 12: Pedant reports

**Files:**
- Modify: `docs/pedant-report.md`, `docs/pedant-extended-report.md`, `docs/claims.toml`

- [ ] **Step 1: Correct the headline status**

Add an as-of banner naming the run date and stating that the body is historical. Replace every "known plain-HTTP artifact" paragraph with the harness-patch explanation and a link to `scripts/run-pedant.sh`. Record the new `headers` result from Task 11. Remove the artifact from the "remaining failure" summaries; keep unrelated trade-offs.

- [ ] **Step 2: Inventory the headline numbers**

Add `manual` claims for the pass/fail/pending totals with `ref` to the report and `checked = <today>`.

- [ ] **Step 3: Run the checker and commit**

Run: `python3 scripts/check-claims.py --no-coverage && python3 scripts/render-remaining-work.py`
Expected: PASS.

```bash
git add docs/pedant-report.md docs/pedant-extended-report.md docs/claims.toml docs/remaining-work.md
git commit -m "docs(pedant): fix the header status and date the reports"
```

---

### Task 13: Regenerate and verify the whole gate

**Files:**
- Modify: `docs/claims.toml`, generated files as needed

- [ ] **Step 1: Re-render and check generated output**

Run: `python3 scripts/render-remaining-work.py && python3 scripts/check-claims.py`
Expected: PASS with no diff after a second render.

- [ ] **Step 2: Run every local suite**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets && cargo test --workspace`
Run: `cd website/homepage && npm test`
Run: `python3 -m unittest discover -s scripts/tests -v`
Expected: all PASS.

- [ ] **Step 3: Confirm the CI definition matches the gate**

Run the YAML parse from Task 2 and read the three workflows to confirm: pedant is schedule/dispatch only, PR jobs install no Ruby, `MICROWAVE_REQUIRE_POSTGRES=1` is set in the PostgreSQL job, and the claims step runs `scripts/check-claims.py`.

- [ ] **Step 4: Run the pedant core and extended tags that the claims name**

Run: `scripts/run-pedant.sh core extended`
Expected: every tag green except the 7 upstream pending examples.

- [ ] **Step 5: Commit any remaining inventory or wording fixes**

```bash
git add -A
git commit -m "docs(claims): finalize inventory and generated work list"
```

---

## Self-Review Notes

- Spec coverage: inventory/renderer/checker (Tasks 1–4, 13), corrections (Tasks 4–12), CI (Task 2), header patch and reports (Tasks 11–12), scope for all docs and website (Tasks 5–10). The spec's "full TODO sweep" is split across Tasks 7–9.
- Review Focus coverage: dead links (Task 3 `link_errors`, Task 6 test), manual expiry (Task 3 test), hand-edited generated regions (Task 1 `replace_block` test, Task 3 `render_errors`), fork/Ruby (Task 2 triggers), unsupported TODO ticks (Tasks 7–9 verification comments).
- Type consistency: `Claim`, `Surface`, `Verify`, `validate`, `coverage_gaps`, `link_errors`, `render_errors`, and the marker constants are used with the same names in every task.
