# README as an operator landing page

Status: draft, awaiting approval
Date: 2026-10-29
Size: small change, short design, no builder check

## Intent

`README.md` currently opens with a hero image and one dense sentence, then 17
engineering bullets. Nothing in the first screen tells a Chef Infra Server
operator whether Microwave can run their existing `chef-client`, and the only
proof of that (the end-to-end converge harness) is a clause at line 30.

The reader this serves is a Chef Infra Server operator evaluating a
migration. The action after reading is: run it locally, watch a real
`chef-client` converge, then keep going or drop it. Rust developers are served
by the existing specs and `crates/` layout, one section further down.

## Goals

- The first screen states what this is, who it is for, and that a stock
  `chef-client` converges against it.
- A copy-paste path reaches a successful converge without reading further, and
  it says what success looks like.
- The limitations an operator would want to know before investing are visible
  above the fold, not 200 lines down.
- The migration route (snapshot export, `migrate-from-legacy`, knife backup
  import) is discoverable from the top.
- Every existing heading, anchor, the generated Remaining Work block, and the
  whole Operational CLI Notes reference stay in place.

## Non-goals

- No new facts, benchmarks, badges, screenshots, or customer stories. Static
  text only; no service calls.
- No website or Rust changes. README, `docs/claims.toml`, the two
  `chef-client` e2e harness files, and one dated note in the e2e report.
- No moving the CLI reference into a new document. That is the larger option
  and can follow later.
- No changes to the hero image itself.

## Anti-goals

- Copy that reads as marketing or claims something the repository cannot
  demonstrate.
- Broken anchor links from `docs/` or the website into README.
- A README that only works when rendered on GitHub.

## Constraints

- `docs/claims.toml` pins 65 README sentences by `needle`. Every reworded
  pinned sentence updates its needle; a pin whose sentence survives keeps its
  text unchanged. No verify gets weaker, and `manual` totals stay `manual`.
- New load-bearing sentences get their own claims with real `file`/`grep`
  verifies against the harness, the deployment guide, or the generated gap
  list.
- English, playful where it costs nothing, factual throughout. MCP stays
  `experimental`. No `AI-powered`, no `built-in AI`.
- The `generated:remaining-work` block is regenerated with
  `scripts/render-remaining-work.py`, never hand-edited.
- One commit, only after the claims suite is green, and only after a real
  `scripts/run-chef-client-e2e.sh` run prints `PASS` with no flags set
  (`chef-client` 18.7.10, `mixlib-authentication` 3.0.10 and Ruby 4.0.9 are
  installed locally).
- The harness stays bash 3.2-safe and uses only the Ruby already required by
  the bootstrap, no new dependencies.

## Approaches

1. **Proof-first operator page** (chosen). New top sections, existing material
   reordered below. ~25 needles change, no new facts.
2. **Minimal recut** (dropped). Fix the alt text, add a TOC, move `Run Locally`
   up. ~8 needles, but a reader still meets no reason to care and no proof.
3. **Migration-story landing** (dropped for now). As 1 plus moving the CLI
   reference to `docs/operations-cli.md`. Best fit for the operator, but ~20
   more pins re-anchor to a new file and the reference leaves the README.

## Decided

1. Lead order: title, English hero alt text, one-line pitch, hero image,
   `## Why`, `## Try it in five minutes`, `## What works`, `## Known gaps`,
   `## Migrate from Chef Infra Server`, then the existing `## Status` bullets
   and everything from `## Run Locally` onward unchanged in order.
2. `## Why` is four lines: drop-in for the Chef Infra Server API, a real
   `chef-client` converge as the proof, the current pedant numbers
   (652 passed / 0 failed of 660 core, 3,284 passed / 0 failed of 3,291
   extended), and the independent-project sentence moved down here from line 5.
3. `## Try it in five minutes` gives prerequisites (Rust, Ruby with
   `mixlib-authentication`, a `chef-client` 18+ binary or
   `MW_E2E_CHEF_CLIENT`), the bare command
   `scripts/run-chef-client-e2e.sh`, and the success signal the harness already
   prints and checks: exit status 0, the `PASS` verdict, the written marker file
   and the saved node. One line notes that `MW_E2E_COMPAT=1` re-enables the two
   provisioning steps for a server that regressed. A second block covers driving
   it yourself with `serve --check` / `serve` and the credentials the first run
   writes to `./data/`.
4. `## What works` is a table: HTTP API surface, signed auth, both metadata
   backends, operations CLI, migration, MCP (experimental). Each row names the
   suite or document that proves it.
5. `## Known gaps` keeps the generated block and adds three operator-relevant
   lines drawn from the existing gap list: no `chef-server.rb` importer, no
   multi-node HA or verified failover, embedded search is a relational scan.
6. `## Migrate from Chef Infra Server` describes only what runs today: JSON
   snapshot export plus `migrate-from-legacy`, or `import-knife-backup`, then a
   `reindex`, with `docs/deployment.md` section 10 as the full route and the
   knife importer's coverage limits named.
7. Hero alt text becomes "A glittering rainbow microwave under neon light".
8. A five-item table of contents follows the pitch.
9. Historical pedant table stays, with the current totals above it and the
   dated report linked as the source.
10. Stale `readme-pedant-core-totals` (651 / 1 / 8) is corrected to the current
    652 / 0 / 8 while the table keeps its recorded baseline.
11. The harness sticks with stock Chef Infra Server behaviour instead of the
    README carrying two flags: `MW_E2E_COMPAT` defaults to `0`,
    `MW_E2E_SYNTH_V2` keeps its `1` default and only applies when compat is on,
    and `MW_E2E_COMPAT=1` re-enables the ACL grant. Upstream
    grants `clients` read on the environments container in
    `?DEFAULT_EC_EXPANDED_ORG` and returns `all_files` for API v2, so the
    unassisted run is the upstream-equivalent one. The harness comments and the
    two 403 / no-`all_files` warnings say so, `docs/chef-client-e2e-report.md`
    gets one dated note that the default flipped and its historical table keeps
    describing the 2026-10-24 run, and the CI comment in `README.md` drops the
    flag pair.

## Left to the builder

Exact wording, how many Status bullets keep their text verbatim, the table
rows' precise phrasings, and whether any current bullet can be tightened
without a claim fight.

## Verification

- `scripts/run-chef-client-e2e.sh` with no flags: `PASS`, exit 0.
- `MW_E2E_COMPAT=1 scripts/run-chef-client-e2e.sh`: still works as the
  opt-in path.
- `python3 scripts/check-claims.py` and
  `python3 scripts/render-remaining-work.py --check`: clean.
- `python3 -m unittest discover -s scripts/tests`: 27 passing.
- `actionlint .github/workflows/*.yml`: clean if a workflow is touched (it is
  not).
