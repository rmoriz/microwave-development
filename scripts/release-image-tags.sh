#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 vX.Y.Z[-prerelease]" >&2
    exit 1
fi

tag="$1"
if [[ ! "$tag" =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "not a release tag: $tag" >&2
    exit 1
fi

version="${tag#v}"
if [ -n "${BASH_REMATCH[4]}" ]; then
    stable=0
else
    stable=1
fi

printf 'version=%s\nmajor_minor=%s.%s\nstable=%s\n' \
    "$version" "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "$stable"
