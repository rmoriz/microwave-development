#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
resolver="$root/scripts/release-image-manifest-tags.sh"
fail=0

assert_output() {
    local expected="$1"
    shift
    local out
    out="$("$resolver" "$@")" || { echo "FAIL: $* exited non-zero"; fail=1; return; }
    if [ "$out" != "$expected" ]; then
        printf 'FAIL: %s\n  expected: %q\n  actual:   %q\n' "$*" "$expected" "$out"
        fail=1
    fi
}

# Newest stable release: version, minor and latest.
assert_output $'1.2.3\n1.2\nlatest' "v1.2.3" "1.2" 1 "v1.2.3" "v1.2.3"
# Older patch re-run: it must not move minor or latest backwards.
assert_output "1.2.2" "v1.2.2" "1.2" 1 "v1.2.3" "v1.2.3"
# Newest patch of an old minor: minor moves, latest belongs to the newer minor.
assert_output $'1.2.3\n1.2' "v1.2.3" "1.2" 1 "v1.3.0" "v1.2.3"
# Pre-release: exact version only.
assert_output "1.2.3-rc.1" "v1.2.3-rc.1" "1.2" 0 "v1.2.3" "v1.2.3"

exit "$fail"
