#!/usr/bin/env bash
#
# Smoke-test a built .deb in throwaway Debian and Ubuntu containers.
#
# Usage:
#   scripts/test-deb.sh [path/to/package.deb]
#
# Without an argument the newest dist/*.deb is used. Override the test images
# with TEST_IMAGES="debian:12-slim" (space-separated).
#
# Each container installs the package through apt, checks the installed
# layout and service account, runs both binaries, validates the configuration
# and then purges the package to exercise the postrm cleanup.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ "$#" -gt 0 ]; then
    DEB="$1"
else
    DEB="$(find dist -maxdepth 1 -name '*.deb' -print 2>/dev/null | sort | tail -n1)"
fi

if [ -z "${DEB:-}" ] || [ ! -f "$DEB" ]; then
    echo "error: no .deb found; run scripts/build-deb.sh first" >&2
    exit 1
fi

DEB_DIR="$(cd "$(dirname "$DEB")" && pwd)"
DEB_NAME="$(basename "$DEB")"
IMAGES="${TEST_IMAGES:-debian:12-slim ubuntu:24.04}"

for image in $IMAGES; do
    echo "==> $DEB_NAME in $image"
    docker run --rm \
        -v "$DEB_DIR:/pkg:ro" \
        -e DEB_NAME="$DEB_NAME" \
        "$image" \
        bash -euc '
            export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq
            apt-get install -y --no-install-recommends "/pkg/$DEB_NAME" >/dev/null

            dpkg -s microwave-server | grep -E "^(Status|Version|Architecture):"
            for binary in microwave-server microwave-ctl chef-server-ctl microwave-migrate; do
                test -x "/usr/bin/$binary"
            done
            test -f /usr/lib/systemd/system/microwave-server.service
            test -f /etc/microwave/microwave.toml
            id microwave >/dev/null
            stat -c "%a %U:%G" /etc/microwave/microwave.toml | grep -qx "640 root:microwave"

            microwave-server version
            microwave-ctl --version
            runuser -u microwave -- microwave-ctl \
                --config /etc/microwave/microwave.toml check-config
            runuser -u microwave -- microwave-server serve \
                --config /etc/microwave/microwave.toml --check

            if command -v systemd-analyze >/dev/null 2>&1; then
                systemd-analyze verify /usr/lib/systemd/system/microwave-server.service
            fi

            apt-get purge -y microwave-server >/dev/null
            test ! -d /var/lib/microwave
            if getent passwd microwave >/dev/null 2>&1; then
                echo "error: service user survived purge" >&2
                exit 1
            fi
        '
    echo "    ok"
done

echo "all images passed"
