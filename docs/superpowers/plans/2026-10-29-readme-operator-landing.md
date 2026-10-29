# README operator landing page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `README.md` into a landing page that lets a Chef Infra Server operator see, in the first screen, that a stock `chef-client` converges against Microwave, run that proof in one command, and read the honest limits — while every sentence stays pinned by `docs/claims.toml`.

**Architecture:** Two halves. First the e2e harness stops defaulting to its compatibility shims, so the bare command is the upstream-equivalent run and `MW_E2E_COMPAT=1` is the opt-in remedy. Then the README gains five new top sections (pitch, TOC, Why, Try it, What works, Known gaps, Migrate) ahead of the existing Status bullets, which keep their text and move down. Claims are written first so the checker fails before the prose exists.

**Tech Stack:** Markdown, the stdlib-only Python claim checker (`scripts/claims.toml`), the bash/Ruby e2e harness, TOML.

**Spec:** `docs/superpowers/specs/2026-10-29-readme-operator-landing-design.md`

## Global Constraints

- English copy, playful where it costs nothing, factual throughout. MCP stays `experimental`. Never write `AI-powered` or `built-in AI`. Writes stay on by default, off with `writes = false`.
- Claim ids match `^[a-z0-9]+(-[a-z0-9]+)*$`; areas and statuses are the fixed sets in `scripts/claims_lib.py`; `manual` claims expire after 90 days and keep `status = "manual"`.
- Every factual claim in the README is either an existing claim or a new one with a real `file`/`grep` verify. No verify gets weaker.
- The `generated:remaining-work` block is regenerated with `python3 scripts/render-remaining-work.py`, never hand-edited.
- Harness code stays bash 3.2-safe (`${ARGS[@]+"${ARGS[@]}"}` under `set -u`) and uses only the Ruby the bootstrap already needs. No new dependency.
- No website and no Rust change.
- One commit per task, imperative scoped subject, only after the task's checks pass.

## Review Focus

- Someone runs the quickstart without `chef-client` or Ruby: the harness exits `2` naming what is missing, not with a shell or Ruby backtrace.
- A future server regression on the default-environment ACL or the v2 `all_files` shape: the unassisted run must fail loudly, with `MW_E2E_COMPAT=1` printed as the remedy, rather than passing on a shim.
- A reworded pinned README sentence whose needle was not updated in the same commit: `python3 scripts/check-claims.py` must catch it.
- TOC entries, heading prose, and any `README.md#…` link resolving after the reorder: anchors must exist and match their headings.
- A reader following the personal-run block on a second terminal: the credentials and port must match what the first run actually writes.

---

### Task 1: Make the e2e harness stock by default

**Files:**
- Modify: `scripts/run-chef-client-e2e.sh:26-29`, `:58`, `:190`, `:274`
- Modify: `scripts/chef-client-e2e-bootstrap.rb:31-32`, `:201`, `:404`
- Modify: `docs/chef-client-e2e-report.md:9`
- Modify: `docs/claims.toml` (append one claim)

**Interfaces:**
- Produces: `MW_E2E_COMPAT` default `0` (stock), `1` re-enables the ACL grant plus, together with `MW_E2E_SYNTH_V2=1`, the v2 manifest synthesis. Both defaults are read in the shell wrapper and again in the bootstrap, so both files agree.
- Produces: claim id `e2e-harness-stock-default`.
- Produces: exit code `2` still means missing Ruby/`mixlib-authentication`/`chef-client`.

- [ ] **Step 1: Write the failing check**

Append to `docs/claims.toml`:

```toml
[[claim]]
id = "e2e-harness-stock-default"
summary = "The chef-client e2e harness runs stock by default; MW_E2E_COMPAT=1 opts into its provisioning shims."
area = "tooling"
status = "verified"
surfaces = [ { path = "scripts/run-chef-client-e2e.sh", needle = "MW_E2E_COMPAT          1 to provision" } ]
verify = { kind = "grep", path = "scripts/run-chef-client-e2e.sh", pattern = "MW_E2E_COMPAT:-0" }
```

- [ ] **Step 2: Run the checker and watch it fail**

Run: `python3 scripts/check-claims.py`
Expected: FAIL, naming `e2e-harness-stock-default` (needle missing, verify pattern `MW_E2E_COMPAT:-0` missing).

- [ ] **Step 3: Flip the defaults in the wrapper**

In `scripts/run-chef-client-e2e.sh` replace the comment block with:

```bash
#   MW_E2E_COMPAT          1 to provision around known server gaps, 0 (default)
#                          to use the server's own responses like a stock
#                          chef-server
#   MW_E2E_SYNTH_V2        1 (default) to synthesize an API v2 manifest when
#                          compat is on and the server returns only the v0 shape
```

and `COMPAT="${MW_E2E_COMPAT:-0}"`. Change the summary line `echo "v2 synthesis: ..."` to print `off unless MW_E2E_COMPAT=1` when `COMPAT` is not `1`.

- [ ] **Step 4: Flip the default and the two warnings in the bootstrap**

`COMPAT = ENV.fetch("MW_E2E_COMPAT", "0") == "1"`. Then the 403 warning becomes:

```ruby
warn "bootstrap: default environment read is 403; the server does not grant clients read. Set MW_E2E_COMPAT=1 to provision the ACE, or the converge will fail"
```

and the synthesis-disabled warning ends with `set MW_E2E_COMPAT=1 MW_E2E_SYNTH_V2=1 to enable it`.

- [ ] **Step 5: Run the checker and watch it pass**

Run: `python3 scripts/check-claims.py`
Expected: PASS (no output).

- [ ] **Step 6: Check the missing-prerequisite path**

Run: `MW_E2E_RUBY=/usr/bin/ruby scripts/run-chef-client-e2e.sh`
Expected: exit `2`, the two-line `cannot load mixlib-authentication` hint, no Ruby backtrace.

- [ ] **Step 7: Run the harness unassisted**

Run: `scripts/run-chef-client-e2e.sh`
Expected: `PASS`, exit 0, and `compat: 0` in the banner.

- [ ] **Step 8: Run the opt-in path**

Run: `MW_SKIP_BUILD=1 MW_E2E_COMPAT=1 scripts/run-chef-client-e2e.sh`
Expected: `PASS`, exit 0, `compat: 1` in the banner.

- [ ] **Step 9: Record the flip in the report**

After the post-merge blockquote at `docs/chef-client-e2e-report.md:9`, add:

```markdown
> **Default flipped (2026-10-29):** the harness now runs stock by default
> (`MW_E2E_COMPAT=0`, stock behaviour as shipped by chef-server) and
> `MW_E2E_COMPAT=1` re-enables both provisioning steps. The table below records
> the 2026-10-24 run under the then-default compatibility mode.
```

- [ ] **Step 10: Commit**

```bash
git add scripts/run-chef-client-e2e.sh scripts/chef-client-e2e-bootstrap.rb docs/chef-client-e2e-report.md docs/claims.toml
git commit -m "test(e2e): run chef-client e2e stock by default"
```

### Task 2: README opening block

**Files:**
- Modify: `README.md:1-37`
- Modify: `docs/claims.toml` (new claims; needles on moved sentences)

**Interfaces:**
- Consumes: the bare command from Task 1 and its `MW_E2E_COMPAT=1` opt-in.
- Produces: claims `readme-why-converge`, `readme-intro-independent`, `readme-toc`, `readme-quickstart-harness`, `readme-quickstart-prerequisites`, `readme-quickstart-success`, `readme-quickstart-compat-optin`, `readme-quickstart-serve`.
- Produces: updated needles on the existing `readme-intro-dropin`, `readme-pedant-core-totals`, `readme-pedant-extended-totals`.

- [ ] **Step 1: Write the failing claims**

Add the eight new claims below. `status = "verified"` throughout; `area` is `docs` except the quickstart rows, which are `tooling`. The needle is the exact sentence fragment the new copy will carry.

| id | area | needle | verify |
| --- | --- | --- | --- |
| `readme-why-converge` | docs | `A stock \`chef-client\` converges against it end to end` | grep `scripts/run-chef-client-e2e.sh` `Runs a real chef-client` |
| `readme-intro-independent` | docs | `not affiliated with or endorsed by Progress` | grep `docs/superpowers/specs/2026-10-24-microwave-server-design.md` `independent project, not affiliated` |
| `readme-toc` | docs | `#try-it-in-five-minutes` | grep `README.md` `#try-it-in-five-minutes` |
| `readme-quickstart-harness` | tooling | `scripts/run-chef-client-e2e.sh` | file `scripts/run-chef-client-e2e.sh` |
| `readme-quickstart-prerequisites` | tooling | `Ruby with \`mixlib-authentication\`` | grep `scripts/run-chef-client-e2e.sh` `mixlib-authentication` |
| `readme-quickstart-success` | tooling | `marker.txt` | grep `scripts/run-chef-client-e2e.sh` `MARKER=.*marker.txt` |
| `readme-quickstart-compat-optin` | tooling | `Set \`MW_E2E_COMPAT=1\`` | grep `scripts/chef-client-e2e-bootstrap.rb` `MW_E2E_COMPAT=1` |
| `readme-quickstart-serve` | deployment | `cargo run -p microwave-server -- serve --check` | grep `bin/microwave-server/src/main.rs` `serve --check` |

Then point the three existing claims at the new sentences: `readme-intro-dropin` at the phrase `designed as a drop-in replacement`, `readme-pedant-core-totals` at `652 passed / 0 failed / 8 pending`, `readme-pedant-extended-totals` at `3,284 passed / 0 failed / 7 pending`. Update those two summaries to `652 passed / 0 failed / 8 pending of 660` and `3,284 passed / 0 failed of 3,291`, keep `status = "manual"`, keep `ref = "docs/pedant-report.md"`, and set `checked = 2026-10-29`.

- [ ] **Step 2: Run the checker and watch it fail**

Run: `python3 scripts/check-claims.py`
Expected: FAIL for every new claim and every re-pointed needle.

- [ ] **Step 3: Rewrite the opening**

Keep `# Microwave Server` and the hero image embed, and put between them: the English alt text `A glittering rainbow microwave under neon light`, one pitch sentence (a Rust reimplementation of the Chef Infra Server API for existing `chef-client` and operations tooling, independent project), the five-item TOC, then the image.

After the image write `## Why` — four lines: drop-in for the API surface, the real converge as proof, the two pedant figures with `docs/pedant-report.md` linked, and why the independent-project sentence matters.

Then `## Try it in five minutes` with a prerequisites line, the bare harness command in a `sh` block, the success signal (exit 0, `PASS`, marker file, saved node), one line that the harness starts its own throwaway server, and the `MW_E2E_COMPAT=1` remedy. A second block shows `cargo run -p microwave-server -- serve --check`, then `serve`, and names `./data/pivotal.pem`, `./data/pivotal.password` and `./data/objectstore.secret` as the credentials the first run writes.

- [ ] **Step 4: Run the checker and watch it pass**

Run: `python3 scripts/check-claims.py`
Expected: PASS.

- [ ] **Step 5: Check the anchors and the copy rules**

Run: `grep -n '^#\|](#' README.md | head -20` and `grep -nE 'AI-powered|built-in AI' README.md`
Expected: every TOC entry has a matching heading; no forbidden phrase.

- [ ] **Step 6: Commit**

```bash
git add README.md docs/claims.toml
git commit -m "docs(readme): lead with the operator proof and a quickstart"
```

### Task 3: README honesty sections

**Files:**
- Modify: `README.md` (after the quickstart, before `## Status`)
- Modify: `docs/claims.toml`

**Interfaces:**
- Consumes: the gap list in `docs/remaining-work.md` and the sections it names.
- Produces: claims `readme-gap-single-node-ha`, `readme-gap-embedded-search-scan`, `readme-migrate-snapshot`, `readme-migrate-knife`, `readme-migrate-reindex`; extra surfaces on `readme-gap-*` and the What-works `readme-*` claims.

- [ ] **Step 1: Write the failing claims**

New claims, all `status = "verified"`, with these needles and verifies:

| id | area | needle | verify |
| --- | --- | --- | --- |
| `readme-gap-single-node-ha` | ha | `Multi-node HA and automatic failover are not validated` | grep `crates/microwave-ctl/src/diagnostics.rs` `Microwave is single-node` |
| `readme-gap-embedded-search-scan` | search | `Embedded search is a relational scan, not an inverted index` | grep `docs/remaining-work.md` `relational scan` |
| `readme-migrate-snapshot` | migration | `migrate-from-legacy --input` | grep `docs/deployment.md` `migrate-from-legacy --input` |
| `readme-migrate-knife` | migration | `import-knife-backup` | grep `docs/deployment.md` `import-knife-backup` |
| `readme-migrate-reindex` | search | `microwave-ctl reindex` | grep `docs/deployment.md` `microwave-ctl reindex` |

Add needles to existing claims for the Known gaps line and the What-works rows: `readme-gap-no-chef-server-rb-importer` → `No \`chef-server.rb\` importer`, `readme-signed-auth` → `\`X-Ops-Sign\` signing`, `readme-two-backends` → `SQLite and PostgreSQL`, `readme-http-status-route` → `\`/server_api_version\``, `readme-mcp-writes-default` → `\`writes = false\``. The migration row is covered by `readme-migrate-snapshot`, so `readme-migration-snapshot` gets no extra surface.

- [ ] **Step 2: Run the checker and watch it fail**

Run: `python3 scripts/check-claims.py`
Expected: FAIL for each new claim and each added needle.

- [ ] **Step 3: Write the three sections**

`## What works` — a table with rows for the signed HTTP API, `X-Ops-Sign` signing, both metadata backends, the operations CLI, migration, and MCP (`experimental`, off by default, writes off with `writes = false`). Each row names its proof: `crates/microwave-api/tests/http_parity_test.rs`, `docs/pedant-report.md`, `crates/microwave-store/tests/dual_backend.rs`, `TODO.md`, `docs/deployment.md`, `docs/mcp.md`. The HTTP row lists `/server_api_version` among its endpoints.

`## Known gaps` — the three limitation lines (no `chef-server.rb` importer, no multi-node HA or verified failover, embedded search is a scan), then the `<!-- generated:remaining-work:start -->` / `end` block moved here unchanged.

`## Migrate from Chef Infra Server` — export a versioned JSON snapshot, `microwave-ctl migrate-from-legacy --input … --yes`, or `import-knife-backup` for a `knife ec backup` tree; note that the knife importer skips artifacts, policies and OAuth applications, and that a migration does not reindex, so run `microwave-ctl reindex` afterwards. Link `docs/deployment.md` section 10.

- [ ] **Step 4: Regenerate the block and run the checker**

Run: `python3 scripts/render-remaining-work.py && python3 scripts/check-claims.py && python3 scripts/render-remaining-work.py --check`
Expected: PASS, no diff after `--check`.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/claims.toml docs/remaining-work.md
git commit -m "docs(readme): show what works, what is missing, and the migration route"
```

### Task 4: Coherence pass and full verification

**Files:**
- Modify: `README.md` (Status bullets, `## Remaining Work`, `## Pedant Compatibility`)

**Interfaces:**
- Consumes: everything above.
- Produces: a README with no leftovers from the old opening and one claim check that is clean from scratch.

- [ ] **Step 1: Remove what the new sections made redundant**

Delete `The repository contains a working vertical slice:` if the Status heading now carries it, the duplicate `The full workspace suite is green.` after the TLS section, and the `<!-- generated:remaining-work:start -->` block from `## Remaining Work` (it moved in Task 3), replacing it with one line pointing at `## Known gaps`. Drop the `MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0` parenthetical from the gate paragraph.

- [ ] **Step 2: Fix the stale baseline sentence**

`## Pedant Compatibility` keeps its table. Above it, repeat the current core figure from `## Why`, link `docs/pedant-report.md`, and say the table is the dated 2026-10-24 baseline.

- [ ] **Step 3: Sweep for stale numbers and duplicate needles**

Run: `grep -n '651\|MW_E2E_COMPAT=0' README.md` and

```bash
python3 - <<'PY'
import collections, tomllib
d = tomllib.load(open("docs/claims.toml", "rb"))
pairs = collections.Counter((c["id"], s["needle"]) for c in d["claim"] for s in c["surfaces"])
print([k for k, n in pairs.items() if n > 1])
PY
```

Expected: `651` only inside the historical table if it is kept, `MW_E2E_COMPAT=0` gone from README, `[]` for duplicate pairs.

- [ ] **Step 4: Run everything**

```bash
python3 scripts/check-claims.py
python3 scripts/render-remaining-work.py --check
python3 -m unittest discover -s scripts/tests
git grep -nE 'AI-powered|built-in AI' -- README.md
```

Expected: checker PASS, render PASS, 27 tests PASS, no forbidden phrase.

- [ ] **Step 5: Read the first screen as the operator**

Run: `sed -n '1,70p' README.md`
Expected: one `h1`, no `##` before the pitch, TOC targets all present, and the quickstart reachable without scrolling past a bullet list.

- [ ] **Step 6: Commit**

```bash
git add README.md
git commit -m "docs(readme): finish the operator landing page"
```
