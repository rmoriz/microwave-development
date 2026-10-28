# Working in this repository

## Layout

- `crates/`, `bin/`: the Rust workspace (server, API, store backends, CLI).
- `website/homepage`: the static Astro marketing site for `microwave.io`.
- `docs/`: operator and contributor documentation. `docs/mcp.md` is the guide
  the homepage links to.
- `docs/superpowers/`: specs and implementation plans.

## Which tests to run

Run the suite for the area you touched, not the whole workspace by default.

| Change | Verify with |
|---|---|
| `website/**` only | `cd website/homepage && npm test` |
| `website/**` and `docs/mcp.md` | the same; the content test asserts guide facts |
| `docs/mcp.md` only | the same; it checks the guide file |
| `crates/**`, `bin/**`, `Cargo.toml` | `cargo test --workspace` |
| Rust + website together | both |

`npm test` runs `astro check`, the built-HTML content tests, and Playwright
with axe. A website-only commit does not need `cargo test`.

Run `cargo fmt --all` and `cargo clippy --workspace --all-targets` before
committing Rust. The homepage has no linter beyond `astro check`.

## Website rules

`website/homepage/README.md` is binding. The short version:

- English copy, playful cooking angle, no invented metrics, customers, or
  certifications.
- Keep every factual claim traceable to the repository or `docs/mcp.md`.
- MCP is always `experimental`. Writes are on by default and switched off with
  `writes = false`. Never write `AI-powered` or `built-in AI`.
- The page must stay useful with JavaScript disabled, avoid horizontal
  overflow at 320/375/768/900/1440 px, and keep one `h1` plus a sane heading
  order.
- No runtime requests, client-side framework, or new dependency.

## Rust notes

- `cargo test` needs no external services for the default suites; Postgres
  tests skip when no database is configured.
- Keep the signed HTTP API and the MCP tools on the same service, validation,
  and ACL paths. MCP is a thin surface over the API, not a second
  implementation.
- MCP tool calls must keep the structured log line: organization, principal,
  tool, duration, result count, outcome, and the target on writes.

## Committing

- Work directly on the current branch unless told otherwise.
- One focused commit per change, imperative subject with a scope
  (`feat(website):`, `fix(mcp):`, `docs(mcp):`).
- Commit only after the relevant suite is green.
