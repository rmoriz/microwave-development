# Docker-e2e: pull the published image or build from source

## Intent

The e2e stack currently defines `microwave` with both `build:` and `image:`.
Compose therefore builds it by default, so a user with only Docker cannot start
the stack against the released server without a checkout build.

The default becomes a pull of `ghcr.io/rmoriz/microwave-server:latest`. A
Source-Build stays available as an explicit Compose override. The workstation
image remains local-only and is still built by Compose on first use.

## Goals

- `docker compose up -d` in `examples/docker-e2e/` pulls the public `latest` image and
  starts the stack without Rust or a large local build.
- `docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build`
  builds `microwave` from the repository root Dockerfile and tags it locally.
- `MW_IMAGE` pins either path; the pull default is the GHCR `latest`, the build
  default is `microwave-server:e2e`.
- `docker compose config` shows exactly one image source per mode.

## Non-goals

- Publishing the workstation, seed or node images.
- Changing the node distro matrix, volumes, ports or e2e fixtures.
- Changing the root production Dockerfile or the release workflow.
- A wrapper script around Compose.

## Anti-goals

- A default that still compiles the server on every `up`.
- A source build that overwrites the pulled `ghcr.io/...:latest` tag locally and
  hides which variant runs.
- Two divergent service definitions that can drift.

## Constraints

- Compose merges scalar fields from later `-f` files, so the override's
  `image:` default must differ from the base default.
- The project name and volumes stay in the base file so `down` and existing
  volumes keep working.
- `latest` is moving; the README says to pin `MW_IMAGE` for reproducibility.

## Approaches

1. Pull-only base file plus a `docker-compose.build.yml` override.
2. One file with `build:` and `image:` plus an `./up` wrapper that chooses
   `--build` or `--no-build`.
3. Profiles with two microwave services and profile-bound dependants.

**Chosen: 1.** Native Compose semantics, no script, and the default really
pulls. 2 keeps the ambiguity in direct Compose calls; 3 duplicates the service
graph.

## Decisions

- **Base file:** `microwave` keeps `image: ${MW_IMAGE:-ghcr.io/rmoriz/microwave-server:latest}`
  and loses its `build:` block.
- **Override:** new `docker-compose.build.yml` adds `build` with the root
  context and the existing `RUST_VERSION` arg, and sets
  `image: ${MW_IMAGE:-microwave-server:e2e}`.
- **README:** the usage section leads with the pull, shows the override command
  for source work, explains the moving `latest`, and keeps the workstation
  build note separate.
- **Config table:** `MW_IMAGE` documents both defaults and the pin form
  `ghcr.io/rmoriz/microwave-server:0.3.1`.
- **Verification:** `docker compose config --images` in both modes, plus a
  `docker compose config --format json` assertion for the `build` block on the
  override.

## Left to the builder

- The exact README wording and placement.
- The config test helper and whether it skips when Docker is unavailable.
- Any top-of-file comments in both Compose files.

Size: small change.
