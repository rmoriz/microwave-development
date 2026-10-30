# Release-built .deb with the systemd unit and log rotation

## Intent

The repository can already build a `.deb` locally, but only with a Rust
toolchain and `dpkg-deb` on the host. The GitHub Release publishes tarballs and
nothing else. This change lets CI build the package on the tag that names the
release and attach it there, so an operator can install a released build on a
Debian/Ubuntu host without a local build.

The package is the surface that carries the systemd unit. That is why it also
carries a logrotate rule: on a systemd host the rotation job that a plain
tarball leaves to the operator belongs in the package.

## Goals

- A tag push produces `microwave-server_<version>_<arch>.deb` for `amd64` and
  `arm64` and uploads it to that tag's GitHub Release, next to the tarballs.
- The package writes its log to `/var/log/microwave/microwave.log` and ships
  `/etc/logrotate.d/microwave-server`, daily, 14 kept, compressed.
- The local `scripts/build-deb.sh` build and the CI build share one packaging
  path; CI runs the same script.

## Non-goals

- No apt repository, no GitHub Pages, no `ghcr.io` image.
- No lintian-clean packaging work.
- No change to the container image or the docker-e2e stack.
- No second, CI-only packaging implementation.

## Anti-goals

- A release that ships a package whose version or contents differ depending on
  where it was built.
- A logrotate rule that silently rotates an empty file because the unit still
  logs to journald.
- A dispatch run that publishes a `.deb` under a version that is not a Debian
  version string.

## Constraints

- The server logs to stderr through `tracing`; there is no file logging and no
  log-file reopen signal (`SIGHUP` reloads TLS only). Rotation therefore has to
  be `copytruncate` against a file systemd appends to.
- `StandardOutput=append:` replaces journald as the application log sink.
  `journalctl -u microwave-server` keeps only unit start/stop output. This is
  accepted.
- GitHub Packages hosts no apt registry; the artifact goes to the Release.
- The build matrix already produces exactly the right binary per architecture,
  so packaging reuses it instead of compiling twice.

## Approaches

1. Package inside the existing matrix job, reusing the binary just built.
2. A separate `package-deb` job downloading the artifacts.
3. A separate `deb.yml` workflow on the same tag.

**Chosen: 1.** Fewest moving parts, correct architecture already present, the
`.deb` rides the existing `dist/*` upload. 2 adds an artifact round trip and a
second checkout; 3 duplicates toolchain setup and races `release.yml` for the
same Release.

## Decisions

- **Log destination:** the unit appends to `/var/log/microwave/microwave.log`
  with `LogsDirectory=microwave`; the logrotate rule uses
  `su microwave microwave` and `copytruncate`. journald keeps unit-level
  messages only, and `docs/deployment.md` says so.
- **Version:** the tag is the source of truth, not `Cargo.toml`.
  `scripts/build-deb.sh` gains a `DEB_VERSION` override; the workflow strips a
  leading `v` and rejects anything that is not a Debian upstream version.
- **Dispatch runs:** the `.deb` job only runs when the ref is a tag. A dispatch
  on a branch still builds tarballs and skips packaging.
- **Dependency:** `logrotate` becomes a hard `Depends`. Recommends would let a
  host silently grow the log without bound.

## Left to the builder

- Step wiring in `release.yml` (`CARGO_TARGET_DIR` per matrix entry, `DEB_ARCH`
  from the matrix, `DEB_VERSION` from the validated tag).
- logrotate rule contents and the privilege model for `copytruncate`.
- Extending `scripts/test-deb.sh` to assert the unit's `StandardOutput=` line,
  the installed rule, and a `logrotate -d` run inside the test container.
- Documentation in `docs/debian-package.md` and the logging section of
  `docs/deployment.md`.

Size: small change.
