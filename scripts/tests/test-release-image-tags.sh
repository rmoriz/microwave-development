#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
resolver="$root/scripts/release-image-tags.sh"
fail=0

assert_output() {
    local tag="$1" expected="$2"
    local out
    out="$("$resolver" "$tag")" || { echo "FAIL: $tag exited non-zero"; fail=1; return; }
    if [ "$out" != "$expected" ]; then
        printf 'FAIL: %s\n  expected: %q\n  actual:   %q\n' "$tag" "$expected" "$out"
        fail=1
    fi
}

assert_output "v1.2.3" $'version=1.2.3\nmajor_minor=1.2\nstable=1'
assert_output "v0.3.0-rc.1" $'version=0.3.0-rc.1\nmajor_minor=0.3\nstable=0'
assert_output "v10.20.30-alpha.2" $'version=10.20.30-alpha.2\nmajor_minor=10.20\nstable=0'

for bad in "1.2.3" "v1.2" "vnext" "v1.2.3.4"; do
    if "$resolver" "$bad" >/dev/null 2>&1; then
        echo "FAIL: $bad should have failed"
        fail=1
    fi
done

exit "$fail"
