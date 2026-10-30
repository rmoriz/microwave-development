#!/usr/bin/env bash
# Version resolution of scripts/build-deb.sh. Runs without Docker or a build.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

fail() { echo "FAIL: $*" >&2; exit 1; }

expected="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"

unset DEB_VERSION
got="$(scripts/build-deb.sh --print-version)"
[ "$got" = "$expected" ] || fail "default should be the workspace version, got '$got'"

got="$(DEB_VERSION=v1.2.3 scripts/build-deb.sh --print-version)"
[ "$got" = "1.2.3" ] || fail "expected 1.2.3, got '$got'"

got="$(DEB_VERSION=2.0.0~rc.1 scripts/build-deb.sh --print-version)"
[ "$got" = "2.0.0~rc.1" ] || fail "expected 2.0.0~rc.1, got '$got'"

DEB_VERSION=not_a_version scripts/build-deb.sh --print-version >/dev/null 2>&1 \
    && fail "an underscore is not a Debian version character"

DEB_VERSION=v scripts/build-deb.sh --print-version >/dev/null 2>&1 \
    && fail "an empty version must fail"

echo "ok"
