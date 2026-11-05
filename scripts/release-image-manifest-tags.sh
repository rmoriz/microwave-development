#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 5 ]; then
    echo "usage: $0 TAG MAJOR_MINOR STABLE HIGHEST_STABLE_TAG HIGHEST_STABLE_SAME_MINOR_TAG" >&2
    exit 1
fi

tag="$1"
major_minor="$2"
stable="$3"
highest_stable="$4"
highest_stable_same_minor="$5"

printf '%s\n' "${tag#v}"

if [ "$stable" = "1" ]; then
    if [ "$tag" = "$highest_stable_same_minor" ]; then
        printf '%s\n' "$major_minor"
    fi
    if [ "$tag" = "$highest_stable" ]; then
        printf 'latest\n'
    fi
fi
