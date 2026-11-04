# Release-built multi-arch container image on GHCR

## Intent

The release workflow already builds the server, CLI and migration binaries
natively for `linux-amd64` and `linux-arm64` and publishes tarballs and `.deb`
files on a `v*` tag. Operators still have to build the container image
themselves, which needs Docker and a Rust toolchain.

This change publishes an official image for both architectures on GHCR on the
same tag. The image reuses the binaries that the matrix job has just built;
nothing is compiled inside Docker.

The release workflow becomes strictly tag-only. A `workflow_dispatch` run has
no tag and therefore builds nothing, so the trigger is removed.

## Goals

- A `vX.Y.Z` tag pushes `ghcr.io/rmoriz/microwave-server:X.Y.Z-amd64` and
  `:X.Y.Z-arm64` from the binaries of the same matrix leg, then a manifest
  list under `X.Y.Z`, `X.Y` and `latest`.
- A pre-release such as `vX.Y.Z-rc.1` pushes only `X.Y.Z-rc.1-amd64`,
  `:X.Y.Z-rc.1-arm64` and the exact manifest; `latest` stays put.
- The image keeps the runtime contract of the current `Dockerfile`: Debian
  slim, UID/GID `10001`, `/var/lib/microwave`, `/etc/microwave/microwave.toml`,
  healthcheck on `/_status`, no entrypoint, `microwave-ctl` default command.
- Every pushed architecture image passes `microwave-server --version` before
  it is pushed.
- OCI labels carry title, description, source, revision, version, creation
  time and licence.

## Non-goals

- No source builds in CI images. The existing local `docker build .` path
  keeps compiling and stays available.
- No image signature, SBOM or provenance attestation. The labels carry the
  commit.
- No changes to the tarball and `.deb` artifacts.
- No docker-e2e or Compose changes; `MW_IMAGE` still defaults to a local tag.

## Anti-goals

- An image whose architecture does not match the binary it contains.
- A failed image build that still publishes the GitHub Release.
- Replacing the local build path with a release-only Dockerfile that operators
  cannot use without prebuilt binaries.
- A silent `latest` on a pre-release.

## Constraints

- GitHub Packages requires `packages: write`; the repository is public and the
  package must be set to public once after the first push.
- `ubuntu-24.04-arm` is already the native arm64 runner in the matrix, so no
  QEMU is needed.
- The binaries are built against Ubuntu 24.04's glibc (2.39); `debian:trixie-slim`
  (glibc 2.41) is the oldest matching release base and is already used.
- `microwave-ctl` resolves `microwave-server` from its own directory, so all
  three binaries must land in `/usr/local/bin`.
- The dirty context is avoided: the release stages the binaries into a temp
  directory and uses that as the Docker context.

## Approaches

1. Build and push the architecture image inside the existing matrix job after
   packaging, then merge the digests in a `manifest` job.
2. A separate `image` matrix job downloads the arch tarball and builds the
   image; a `manifest` job merges.
3. One Buildx job builds both platforms under QEMU.

**Chosen: 1.** It has no artefact round trip, runs natively on both
architectures, and reuses the binaries in the same job that already knows the
architecture. The cost is a fuller `build` job and `packages: write` on it. 2
adds an artifact round trip and a second checkout for the same result; 3 pays
emulated `apt-get` for arm64 and is the slowest.

## Decisions

- **Dockerfile:** one file with a shared `runtime` stage. `local` keeps the
  Rust builder and stays the default target; `release` copies the staged
  binaries and is the only target CI uses. `FROM builder` is pruned for
  `--target release`, so the Rust image is never pulled there. The common
  runtime contract lives in `runtime`, not in two files that can drift.
- **Tags:** git tag without the leading `v` is the version. Stable is
  `^v[0-9]+\.[0-9]+\.[0-9]+$`; pre-releases are other valid semver tags. The
  `meta` job rejects any other tag before the build, so a non-semver tag
  produces no artifacts at all.
- **Manifest:** the `manifest` job runs on `ubuntu-24.04` after both matrix
  legs, logs in to GHCR, and runs `docker buildx imagetools create` against
  the two pushed arch tags. It is
  the only job allowed to write the exact, `X.Y` and `latest` tags.
- **Release gate:** `publish` needs `[build, manifest]`. The GitHub Release is
  only created or updated after the manifest exists.
- **Smoke test:** `docker run --rm <arch-image> microwave-server --version`
  runs before `docker push`. A failing binary never reaches GHCR.
- **Version mismatch:** the tag stays the source of truth. A tag that differs
  from `workspace.package.version` emits a warning, as it already does for the
  `.deb`.
- **Package visibility:** documented as a one-time manual step in
  `docs/deployment.md`; the image is not usable anonymously before it.
- **Local path:** `docs/deployment.md` keeps `docker build -t
  microwave-server:local .` and gains the `ghcr.io` pull next to it.

## Left to the builder

- `meta` job and its tag validation, version derivation, `major.minor` and
  `latest` outputs.
- `build` matrix step order: package, upload artifact, then stage the image
  context, build, smoke, login, push.
- Passing `VERSION`, `REVISION`, `CREATED` and `SOURCE` as build args to the
  label block.
- `manifest` job with `docker/setup-buildx-action` and `imagetools create` for
  the tag set from `meta`.
- `docs/deployment.md` section for pulling the image and the public-package
  note; README pointer if it lists artifacts.
- `scripts/tests` coverage where the workflow linter cannot express the tag
  rules.

Size: small change.
