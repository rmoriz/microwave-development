# Debian and Ubuntu packages

The repository contains a minimal, local `.deb` packaging layer. It produces a
single binary package, `microwave-server`, from the Cargo workspace. It is not
a signed APT repository and it is not lintian-clean; it is meant for installing
builds on your own Debian/Ubuntu hosts.

## Build

```sh
scripts/build-deb.sh
```

The result is `dist/microwave-server_<version>_<arch>.deb`. In CI the version
comes from the release tag (one leading `v` is stripped); a local build takes it
from `[workspace.package]` in `Cargo.toml`. The architecture is derived from the
host (`amd64`, `arm64`, `armhf`), or from `DEB_ARCH`.

On Linux the script builds natively. On macOS or another non-Linux host it
re-executes itself in Docker (`BUILD_IMAGE`, default `rust:1-bookworm`) because
the binaries have to be linked against Linux glibc. Rebuilds reuse the named
volumes `microwave-deb-cargo-registry` and `microwave-deb-cargo-git`.

Useful overrides:

| Variable | Meaning |
| --- | --- |
| `BUILD_IMAGE` | Builder image for non-Linux hosts (`rust:1-bookworm`) |
| `DEB_ARCH` | Debian architecture instead of `uname -m` |
| `DEB_MAINTAINER` | `Maintainer:` field (defaults to `git config user.*`) |
| `DEB_OUTPUT_DIR` | Output directory, default `dist` |
| `CARGO_TARGET_DIR` | Cargo target directory, default `target` |
| `DEB_VERSION` | Package version instead of `[workspace.package]` (one leading `v` is stripped) |
| `--print-version` (argument) | Print the resolved version and exit |

`scripts/build-deb.sh --skip-build` reuses binaries that are already in
`target/release`; it is useful while iterating on the packaging metadata.

### glibc baseline

The package declares `libc6 (>= <glibc of the builder>)` because the linker
embeds versioned glibc symbols. With the default `rust:1-bookworm` image the
package installs on Debian 12+ and Ubuntu 24.04+. For Ubuntu 22.04, build in a
22.04-based image that provides Rust and set `BUILD_IMAGE` accordingly.

### Release builds

`.github/workflows/release.yml` runs on `v*` tag pushes and on manual
`workflow_dispatch`. A tag push builds `microwave-server_<version>_<arch>.deb`
and the checksum `microwave-server_<version>_<arch>.deb.sha256` for `amd64` and
`arm64` and attaches both to that GitHub Release, next to the tarballs. A
`workflow_dispatch` on a tag packages; on a branch it produces only the
tarballs — no package. A `v*` tag whose remainder is not a Debian version emits
a warning and ships tarballs only. The package version is the tag's version; the
run warns when it differs from `[workspace.package]`.

## Install

```sh
sudo apt install ./dist/microwave-server_0.1.0_amd64.deb
```

The postinst creates the `microwave` system user and group, creates
`/var/lib/microwave` (0700), tightens `/etc/microwave/microwave.toml` to 0640
`root:microwave`, enables `microwave-server.service` and tries to start it. A
failed first start (for example a busy port) does not fail the installation.

## Installed layout

| Path | Content |
| --- | --- |
| `/usr/bin/microwave-server` | HTTP daemon |
| `/usr/bin/microwave-ctl` | Operational CLI |
| `/usr/bin/chef-server-ctl` | Compatibility alias for the CLI |
| `/usr/bin/microwave-migrate` | Snapshot export/import |
| `/usr/lib/systemd/system/microwave-server.service` | Hardened systemd unit |
| `/etc/microwave/microwave.toml` | Conffile with the local single-node defaults |
| `/etc/logrotate.d/microwave-server` | Conffile with the daily rotation rule |
| `/var/lib/microwave` | Data directory (SQLite database, bookshelf, generated keys) |
| `/var/log/microwave/microwave.log` | Service log, rotated by `/etc/logrotate.d/microwave-server` |
| `/usr/share/doc/microwave-server/` | README, LICENSE, Debian copyright |

The unit is a separate copy of `packaging/systemd/microwave-server.service`
with `/usr/bin` paths, because the repository unit targets a manual install
under `/usr/local/bin`.

## Operate

```sh
systemctl status microwave-server
journalctl -u microwave-server -f
sudo microwave-ctl --config /etc/microwave/microwave.toml status
sudo systemctl restart microwave-server
```

On a packaged host `journalctl -u microwave-server` shows only unit lifecycle
lines; the application log is `/var/log/microwave/microwave.log`.

The unit's stdout and stderr go to `/var/log/microwave/microwave.log` via
`StandardOutput=append:`. systemd opens that file itself as root, so it would
create it root-owned when it is absent; the postinst pre-creates
`/var/log/microwave` 0750 `microwave:microwave` and the log file 0600
`microwave:microwave` so the path and its ownership are stable. It also opens
the file before it makes the `LogsDirectory=`, so a fresh start would otherwise
fail on the missing path. Removing the file and restarting recreates it
root-owned, which is harmless because rotation runs as root either way. journald
therefore only carries the unit's own lifecycle messages (start, stop, reload,
exit and failure); the application log is the file.

`/etc/logrotate.d/microwave-server` rotates the file daily, keeps 14 compressed
generations with `delaycompress`, and uses `copytruncate` because the server has
no log-reopen signal. It runs as root (`su root root`). A `systemctl reload` does
not rotate; use `logrotate -f /etc/logrotate.d/microwave-server` when you need
it now. The `logrotate` package is a hard dependency, so the rule installs with
the package.

Configuration changes outside the TLS certificate pair require a restart.
`systemctl reload microwave-server` sends SIGHUP, which only reloads the TLS
certificate and key.

Put environment overrides in a systemd drop-in rather than editing the unit:

```ini
# /etc/systemd/system/microwave-server.service.d/override.conf
[Service]
Environment=MW_DATABASE__DRIVER=postgres
```

## Upgrade and removal

```sh
sudo apt install ./dist/microwave-server_0.2.0_amd64.deb   # upgrade
sudo apt remove microwave-server                           # keep /var/lib/microwave
sudo apt purge microwave-server                            # remove data and user
```

`/etc/microwave/microwave.toml` is a conffile: local edits survive upgrades and
dpkg asks before replacing them. `/etc/logrotate.d/microwave-server` is a
conffile too, with the same upgrade behaviour, and both survive a plain
`apt remove`, as does `/var/log/microwave`. A purge removes `/var/lib/microwave`,
`/var/log/microwave` and with it the rotated generations (logrotate's own state
lives in `/var/lib/logrotate/status`), `/etc/microwave`, the logrotate rule and
the `microwave` user/group.

## Test

```sh
scripts/test-deb.sh
```

The script installs the newest `dist/*.deb` in `debian:12-slim` and
`ubuntu:24.04`, checks the file layout and permissions, runs both binaries,
validates the configuration as the `microwave` user, exercises rotation, removes
and reinstalls the package to check what a reinstall needs survives, and purges
it to exercise the postrm cleanup. Override the images with
`TEST_IMAGES="debian:12-slim"`.
