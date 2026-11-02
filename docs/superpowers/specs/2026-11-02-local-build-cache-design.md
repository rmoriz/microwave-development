# Local build cache (kache) — design

## Intent

Contributors who want faster rebuilds can opt into
[Kache](https://github.com/kunobi-ninja/kache), a compiler cache that wraps
`rustc`. Everyone else builds exactly as today. Local development only: no CI
job changes.

## Goals

- One documented, copy-pasteable setup path in the README.
- A note in `AGENTS.md` so agents do not "fix" the unexpected `RUSTC_WRAPPER`.
- An escape hatch for the odd command that must not be cached.
- Facts stay traceable to the repository or to Kache's own documentation.

## Non-goals

- No `kache-action` in GitHub Actions; the workflows keep
  `Swatinem/rust-cache`.
- No committed `.kache.toml`, so a contributor's global kache settings (remote
  S3, store size) keep applying.
- No wrapper script, no `[profile]` changes, no new dependency.

## Decisions

- The setup goes in a new README `## Development` section above `## Layout`, and
  the table of contents gains the matching entry.
- The examples use Kache's stable channel (`brew install
  kunobi-ninja/kunobi/kache`, `cargo install kache --locked`, Nix). The v2
  preview is mentioned only through the upstream installation link; the
  upstream docs pin release candidates themselves.
- `cargo install` needs Rust 1.95 or newer while the workspace pins
  `rust-version = "1.94"`, so the README says so and leads with the package
  managers.
- Two claims in `docs/claims.toml` pin the new README statements: one greps
  the `AGENTS.md` note, one greps the workspace `rust-version`.

## Verification

- `python3 scripts/check-claims.py`
- `python3 -m unittest discover -s scripts/tests`
- Install Kache on this machine and run `kache doctor --probe`, so the
  documented commands are known-good. Kache stays installed afterwards.
