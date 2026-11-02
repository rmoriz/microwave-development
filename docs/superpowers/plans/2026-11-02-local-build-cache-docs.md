# Local build cache docs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Document Kache as an optional local compiler cache in the README and `AGENTS.md`, with claims that keep the statements traceable.

**Architecture:** Two documentation edits. `AGENTS.md` gets the agent-facing note first, because a README claim verifies against it. The README gets a `## Development` section, a table-of-contents entry, and two `docs/claims.toml` entries. No code, no workflow, no config file.

**Tech Stack:** Markdown, Python claim checker (`scripts/check-claims.py`), Homebrew for the install check.

**Spec:** `docs/superpowers/specs/2026-11-02-local-build-cache-design.md`

## Global Constraints

- English copy, no invented metrics, customers, or certifications. No `AI-powered` or `built-in AI`.
- Kache stays optional: every step works with it absent.
- The stable channel in examples; the v2 preview only through the upstream installation link.
- `AGENTS.md` is not covered by `COVERAGE_PATTERNS`, so it needs no claim entry.
- Run `python3 scripts/check-claims.py` and `python3 -m unittest discover -s scripts/tests` before each commit.

## Review Focus

- A contributor without Kache must see an unchanged build (plain `cargo`).
- `cargo install kache --locked` fails on Rust 1.94, so the README must lead with package managers.
- The 90-day manual-claim expiry must not bite the factual pairs: the Rust
  floor and the preview flag have grep claims where possible and manual claims
  that point back at the spec.
- Disabling must be obvious and scoped to one command.
- Upstream links must resolve, not be guessed.

---

### Task 1: Agent note in `AGENTS.md`

**Files:**
- Modify: `AGENTS.md` (`## Rust notes`, after the MCP log-line bullet)

**Interfaces:**
- Produces: the literal phrase `Local builds may run through`, which Task 2's claim greps.

- [ ] **Step 1: Add the bullet**

Append to `## Rust notes`:

```markdown
- Local builds may run through [kache](https://github.com/kunobi-ninja/kache):
  it configures Cargo for every project on the machine and sits in
  `RUSTC_WRAPPER`; plain `cargo` commands stay the same. Prefix a command with
  `KACHE_DISABLED=1` when it must not touch the cache.
```

- [ ] **Step 2: Verify the claims still pass**

Run: `python3 scripts/check-claims.py`
Expected: exit 0, no output.

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md
git commit -m "docs(agents): note the optional kache build wrapper"
```

### Task 2: README section and claims

**Files:**
- Modify: `README.md` (table of contents after the `Pedant compatibility` entry; new `## Development` section before `## Layout`)
- Modify: `docs/claims.toml` (append four `[[claim]]` records)

**Interfaces:**
- Consumes: the `AGENTS.md` phrase `Local builds may run through` from Task 1.
- Produces: README heading `### Faster local builds`, claim ids `readme-local-build-cache`,
  `readme-kache-rust-floor`, `readme-kache-rust-upstream-floor` and `readme-kache-probe-preview`.

- [ ] **Step 1: Add the table-of-contents entry**

After `- [Pedant compatibility](#pedant-compatibility)`:

```markdown
- [Development](#development)
```

- [ ] **Step 2: Add the `## Development` section before `## Layout`**

````markdown
## Development

Build from source for local work. The workspace is a plain Cargo workspace; [`AGENTS.md`](AGENTS.md)
lists the suites to run per area and the rules the code follows.

### Faster local builds

[Kache](https://github.com/kunobi-ninja/kache) is an optional compiler cache. It wraps `rustc`,
keeps compile outputs, and restores them on later builds, so a rebuild after a branch switch or a
second worktree does not start from scratch. It is a local tool: nothing needs it to build.

Install it with your package manager and enable it once. `kache init` configures Cargo for every
project in your account, so nothing here sets it up per repository; preview the changes with
`kache init --check`, and add `--yes` when no terminal can answer its prompts:

```sh
brew install kunobi-ninja/kunobi/kache   # apt, AUR, nix and mise also work
kache init
```

Cargo installs need Rust 1.95 or newer, above this workspace's 1.94 floor, so prefer a package
manager. Open a new terminal, then check the setup:

```sh
kache doctor
```

It should end with `All checks passed.` A `kache doctor --probe` run goes further by compiling a
throwaway library twice to prove a restore, but `--probe` only exists in the v2 preview and the
stable Homebrew formula is still 1.x, so the plain command above is what the documented install
gives you. If a dotfile manager symlinks `~/.zshrc`, `kache init` skips the shell parts and says
so; Cargo caching is still configured.

Keep using `cargo` as usual; `kache stats` reports hits and misses. Install routes, daemon and
store settings are in the [Kache documentation](https://kunobi.ninja/docs/kache/getting-started/installation).
Disable the cache for one command with `KACHE_DISABLED=1 cargo test`.
````

The block above is the README text verbatim; `--probe` is v2-preview-only, so the stable formula
gets plain `kache doctor`.

- [ ] **Step 3: Add the claims**

Append to `docs/claims.toml`:

```toml
[[claim]]
id = "readme-local-build-cache"
summary = "README: kache is an optional local compiler cache for developers."
area = "tooling"
status = "verified"
surfaces = [
  { path = "README.md", needle = "optional compiler cache" },
]
verify = { kind = "grep", path = "AGENTS.md", pattern = "Local builds may run through" }

[[claim]]
id = "readme-kache-rust-floor"
summary = "README: the workspace's Rust 1.94 floor is below kache's 1.95 cargo requirement."
area = "tooling"
status = "verified"
surfaces = [
  { path = "README.md", needle = "Cargo installs need Rust 1.95 or newer" },
]
verify = { kind = "grep", path = "Cargo.toml", pattern = "rust-version = \"1.94\"" }

[[claim]]
id = "readme-kache-rust-upstream-floor"
summary = "kache's installation docs require Rust 1.95 or newer for `cargo install`."
area = "tooling"
status = "manual"
surfaces = [
  { path = "README.md", needle = "Cargo installs need Rust 1.95 or newer" },
]
verify = { kind = "manual", ref = "docs/superpowers/specs/2026-11-02-local-build-cache-design.md", checked = 2026-11-02 }

[[claim]]
id = "readme-kache-probe-preview"
summary = "`kache doctor --probe` only exists in the v2 preview and the stable Homebrew formula is still 1.x."
area = "tooling"
status = "manual"
surfaces = [
  { path = "README.md", needle = "only exists in the v2 preview" },
]
verify = { kind = "manual", ref = "docs/superpowers/specs/2026-11-02-local-build-cache-design.md", checked = 2026-11-02 }
```

Also update the plan's own constraints so they match: the two manual claims need
the spec as `ref`, and the 90-day expiry applies to them.

- [ ] **Step 4: Run the claim and workflow checks**

Run: `python3 scripts/check-claims.py`
Expected: exit 0, no output.

Run: `python3 -m unittest discover -s scripts/tests`
Expected: `OK`.

- [ ] **Step 5: Prove the documented commands exist**

Run: `brew install kunobi-ninja/kunobi/kache && kache --version && kache doctor --probe`
(the flag is v2-preview-only; the stable formula uses plain `kache doctor`)
Expected: a version string, then a passing probe report. Record anything the probe says that the
README should warn about, and fix the copy in Step 2 if needed.

- [ ] **Step 6: Commit**

```bash
git add README.md docs/claims.toml docs/superpowers/specs/2026-11-02-local-build-cache-design.md docs/superpowers/plans/2026-11-02-local-build-cache-docs.md
git commit -m "docs(readme): document the optional local build cache"
```
