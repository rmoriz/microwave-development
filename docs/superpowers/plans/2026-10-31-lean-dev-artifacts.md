# Lean Dev Artifacts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop local debug builds from costing 18 GB by turning off debug info and incremental state in the `dev` profile, and clear the artifacts that already exist.

**Architecture:** One profile block in the workspace `Cargo.toml`. `test` inherits `dev` (integration tests included), so `cargo test` gets the lean setting without a second override or an env var; `bench` inherits `release` and is untouched. The stale 24 GB `target/` is removed in the same change, so the full rebuild happens once at the new profile and the before/after numbers are honest.

**Tech Stack:** Cargo profiles, `cargo test`/`clippy`/`fmt` as the gate. No code, no dependencies.

**Spec:** `docs/superpowers/specs/2026-10-31-lean-dev-artifacts-design.md`

## Global Constraints

- Profiles belong in the workspace root `Cargo.toml`; member manifests must not gain `[profile.*]` tables.
- Leave `[profile.release]` untouched (`strip = "symbols"`, `lto = "thin"`).
- `rust-version = "1.94"`; `debug`, `incremental` and `strip` are all stable there.
- Comment style follows the existing release block: state the reason and what is traded away.
- Gate: `cargo fmt --all` and `cargo clippy --workspace --all-targets` clean, `cargo test --workspace` green.

## Review Focus

- `scripts/run-pedant.sh` and `docs/` refer to `target/debug/microwave-server`; `debug = 0` must not move the output path.
- `cargo clippy --workspace --all-targets` builds binaries and test targets at `debug = 0`; link failures from missing symbols would show up there, not in `cargo test`.
- Postgres tests skip without a database: a green run with skips must not be read as full coverage.
- `incremental = false` also applies to `cargo test`, so a second consecutive run is a full rebuild, not a warm one — the plan must not use run time as a pass/fail signal.
- A reviewer sees only a one-line profile change; the commit message has to carry the measured before/after, so the change can be reverted on evidence.

---

### Task 1: Turn off dev debug info and incremental state

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]` ends, then `[profile.release]` at the file end)

**Interfaces:**
- Consumes: nothing.
- Produces: `[profile.dev]` with `debug = 0` and `incremental = false`, inherited by `test`.

- [ ] **Step 1: Record the baseline**

```bash
du -sh target target/debug target/debug/deps target/debug/incremental; df -h . | tail -1
```

Expected: roughly 18 GB total, ~17 GB in `target/debug`, ~17 GB in `deps`, ~6.6 GB in `incremental`. Keep the numbers for the commit message.

- [ ] **Step 2: Add the profile above `[profile.release]`**

```toml
# Local dev and test builds carry no debug info and keep no incremental state:
# DWARF is most of an rlib (four fifths in the biggest one) and incremental
# snapshots add gigabytes on top. Backtraces lose file and line numbers and
# lldb cannot step; release builds are unaffected.
[profile.dev]
debug = 0
incremental = false
```

- [ ] **Step 3: Verify the profile parses and applies**

```bash
cargo build -p microwave-ctl --quiet && ls -l target/debug/microwave-ctl
```

Expected: build succeeds, binary is well under the previous 51 MB (a few MB).

- [ ] **Step 4: Clear the stale artifacts**

```bash
cargo clean
du -sh target 2>/dev/null; df -h . | tail -1
```

Expected: `target` gone or a few MB, disk free up by roughly 18 GB.

- [ ] **Step 5: Run the suite at the new profile**

```bash
cargo test --workspace
```

Expected: PASS. Postgres tests skip without a database; that is the normal local state.

- [ ] **Step 6: Run the linters**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git status --short
```

Expected: no clippy warnings, `Cargo.toml` and the spec are the only changes.

- [ ] **Step 7: Measure the new footprint**

```bash
du -sh target target/debug target/debug/deps target/debug/incremental 2>/dev/null; df -h . | tail -1
```

Expected: `target` a few GB instead of 18 GB and no `incremental` directory.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml docs/superpowers/specs/2026-10-31-lean-dev-artifacts-design.md docs/superpowers/plans/2026-10-31-lean-dev-artifacts.md
git commit -m "build(dev): drop debug info and incremental state

Local target/ reached 18 GB on a volume with 20 GB free: the dev profile
kept full DWARF (119 MB of 154 MB of object bytes in
libmicrowave_api-*.rlib, which was stored three times) and 6.6 GB of
incremental snapshots. target/debug is now <measured> GB.

debug = 0 and incremental = false apply to test and bench through the dev
profile. Backtraces lose file and line numbers, lldb cannot step, and a
repeat test run is a full rebuild. The release profile is untouched."
```

---

## Self-Review

- Spec coverage: `debug = 0` (step 2), `incremental = false` (step 2), comment (step 2), `cargo clean` in the same change (step 4), before/after numbers (steps 1, 7, 8), test gate (step 5), lint gate (step 6). No gaps.
- Step scan: every step has one action and one checkable result; no step says "handle X" without a command.
- Type consistency: no types; the profile key names are the ones Cargo documents (`debug`, `incremental`).
- Review Focus: each line is pinned by a step — output path (step 3), clippy link under `--all-targets` (step 6), skips (step 5), rebuild-not-warm (step 7 wording), commit evidence (step 8).
- Proportion: plan is shorter than the spec's worth of code, since the change is eight lines of TOML.
