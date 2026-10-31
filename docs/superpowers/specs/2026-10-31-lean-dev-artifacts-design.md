# Lean dev artifacts

## Why

`target/` grew to 18 GB on a 926 GB volume with 20 GB free. 17 GB sits in
`target/debug`: the dev profile keeps full DWARF in every rlib, test binary
and binary. Measured in `libmicrowave_api-*.rlib`, 119 MB of 154 MB of object
bytes is debug info (77%), and the same crate is stored three times (build,
test and clippy unit hashes). `incremental/` holds another 6.6 GB. Only
`[profile.release]` is tuned; nothing caps what is built locally.

## Goals

- Local debug builds stop costing double-digit gigabytes.
- The setting applies to `cargo test` too, so the suite does not rebuild the
  pile it just cleaned.
- Nobody has to remember an env var; a plain `cargo build`/`cargo test` is lean.

## Non-goals

- Changing the release profile. It already strips and uses thin LTO.
- Changing CI. The change also helps CI, but no workflow job gets special-cased.
- Toolchain, target-dir layout, `sccache`, or dependency changes.

## Anti-goals

- Slowing the edit-compile loop enough that people work around the profile.
- Hiding the reason: a bare `debug = 0` with no comment repeats the blank
  release profile this replaces.

## Constraints

- `test` inherits `dev` (and integration tests use `test`), so `dev` is the
  only place to set it. `bench` inherits `release` and is untouched; the
  workspace has no benches anyway.
- `cargo clippy --workspace --all-targets` must stay warning-free and
  `cargo fmt --all` clean (AGENTS.md).
- No new dependency, script or config file; `Cargo.toml` is the surface.

## Approaches

1. **`debug = 0` + `incremental = false` in `[profile.dev]`** — chosen. Biggest
   cut, and it stops `incremental/` from regrowing.
2. `debug = 0` only — same space figure, keeps incremental, regrows a few GB.
3. `debug = 1` plus `debug = 0` for dependencies — keeps backtrace line
   numbers, saves roughly half. Rejected: the ask was the maximum.

## Decisions

- `[profile.dev]` gets `debug = 0` and `incremental = false`, with a comment
  stating both the reason and what is given up (`file:line` in backtraces,
  `lldb` stepping, incremental rebuilds), mirroring the release comment.
- Verified with `cargo test --workspace` and a `df`/`du` before and after; the
  existing 18 GB is cleared with `cargo clean` as part of the change, so the
  rebuild happens once, at the new profile.
- A stale `target/` that predates the profile starts from zero anyway, so the
  cleanup is not a separate chore.

## Left to the builder

- The exact comment wording.
- Whether the temporary before/after numbers go in the commit message (they
  should: the change is invisible in review otherwise).
