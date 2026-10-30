#!/usr/bin/env bash
#
# Build a Debian/Ubuntu .deb from this workspace.
#
# On Linux the script builds natively. On other hosts it re-executes itself in
# a builder container, because the binaries must be linked against Linux glibc.
#
# Usage:
#   scripts/build-deb.sh [--skip-build] [--print-version]
#
#   --print-version   Print the resolved package version and exit
#
# Environment:
#   BUILD_IMAGE       Builder image for non-Linux hosts
#                     (default: rust:1-bookworm)
#   DEB_ARCH          Override the Debian architecture (default: from uname -m)
#   DEB_MAINTAINER    Maintainer field (default: git identity)
#   DEB_VERSION       Package version (default: [workspace.package] version
#                     from Cargo.toml; one leading "v" is stripped)
#   DEB_OUTPUT_DIR    Output directory, relative to the repository root
#                     (default: dist)
#   CARGO_TARGET_DIR  Cargo target directory (default: target)
#
# The package is written to dist/microwave-server_<version>_<arch>.deb.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SKIP_BUILD=0
PRINT_VERSION=0
for argument in "$@"; do
    case "$argument" in
        --skip-build) SKIP_BUILD=1 ;;
        --print-version) PRINT_VERSION=1 ;;
        -h|--help)
            sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "error: unknown argument: $argument" >&2
            exit 2
            ;;
    esac
done

VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
if [ -z "$VERSION" ]; then
    echo "error: could not read version from [workspace.package] in Cargo.toml" >&2
    exit 1
fi

if [ -n "${DEB_VERSION:-}" ]; then
    VERSION="${DEB_VERSION#v}"
fi
VALID=0
case "$VERSION" in
    ''|*[!0-9A-Za-z.+~-]*|[!0-9]*) ;;
    *) VALID=1 ;;
esac
if [ "$VALID" -eq 0 ]; then
    echo "error: '${DEB_VERSION:-$VERSION}' is not a Debian version" >&2
    exit 1
fi

if [ "$PRINT_VERSION" -eq 1 ]; then
    echo "$VERSION"
    exit 0
fi

if [ -z "${DEB_ARCH:-}" ]; then
    case "$(uname -m)" in
        x86_64|amd64) DEB_ARCH=amd64 ;;
        aarch64|arm64) DEB_ARCH=arm64 ;;
        armv7l|armhf) DEB_ARCH=armhf ;;
        *)
            echo "error: unsupported architecture: $(uname -m) (set DEB_ARCH)" >&2
            exit 1
            ;;
    esac
fi

if [ -z "${DEB_MAINTAINER:-}" ]; then
    maintainer_name="$(git config user.name 2>/dev/null || true)"
    maintainer_email="$(git config user.email 2>/dev/null || true)"
    if [ -n "$maintainer_name" ] && [ -n "$maintainer_email" ]; then
        DEB_MAINTAINER="$maintainer_name <$maintainer_email>"
    else
        DEB_MAINTAINER="Microwave Maintainers <maintainers@microwave.invalid>"
    fi
fi

if [ "$(uname -s)" != "Linux" ] && [ "${MW_DEB_IN_CONTAINER:-0}" != "1" ]; then
    if ! command -v docker >/dev/null 2>&1; then
        echo "error: building outside Linux requires Docker" >&2
        exit 1
    fi
    echo "==> building in ${BUILD_IMAGE:-rust:1-bookworm} (host is $(uname -s))" >&2
    exec docker run --rm \
        -v "$ROOT:/work" \
        -w /work \
        -e MW_DEB_IN_CONTAINER=1 \
        -e CARGO_TARGET_DIR=/work/target/deb-linux \
        -e DEB_ARCH \
        -e DEB_MAINTAINER \
        -e DEB_VERSION \
        -e DEB_OUTPUT_DIR="${DEB_OUTPUT_DIR:-dist}" \
        -v microwave-deb-cargo-registry:/usr/local/cargo/registry \
        -v microwave-deb-cargo-git:/usr/local/cargo/git \
        "${BUILD_IMAGE:-rust:1-bookworm}" \
        bash scripts/build-deb.sh "$@"
fi

for tool in cargo dpkg-deb; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: $tool is required (run on Debian/Ubuntu or inside Docker)" >&2
        exit 1
    fi
done

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

if [ "$SKIP_BUILD" -eq 0 ]; then
    echo "==> cargo build --release --locked --bins" >&2
    cargo build --release --locked --bins
fi

BINARIES=(microwave-server microwave-ctl microwave-migrate)
for binary in "${BINARIES[@]}"; do
    if [ ! -x "$TARGET_DIR/release/$binary" ]; then
        echo "error: missing $TARGET_DIR/release/$binary (run without --skip-build)" >&2
        exit 1
    fi
done

# The linker embeds versioned glibc symbols. Declaring the builder's glibc
# version keeps dpkg from installing the package on an older release, where
# the dynamic loader would fail at startup.
LIBC_DEP=""
if ldd --version 2>/dev/null | head -n1 | grep -qi glibc; then
    libc_version="$(ldd --version 2>/dev/null | head -n1 | awk '{print $NF}')"
    if [ -n "$libc_version" ]; then
        LIBC_DEP=" (>= $libc_version)"
    fi
fi

PKG=microwave-server
BUILD_ROOT="$ROOT/target/deb"
STAGE="$BUILD_ROOT/${PKG}_${VERSION}_${DEB_ARCH}"
DOC_DIR="$STAGE/usr/share/doc/$PKG"

rm -rf "$STAGE"
install -d "$STAGE/DEBIAN" "$STAGE/usr/bin" \
    "$STAGE/usr/lib/systemd/system" "$STAGE/etc/microwave" "$STAGE/etc/logrotate.d" "$DOC_DIR"

# chef-server-ctl is the same executable as microwave-ctl, so ship it as a
# symlink: one copy of the CLI in the package, same argv[0] name for the alias.
for binary in microwave-server microwave-ctl microwave-migrate; do
    install -m 0755 "$TARGET_DIR/release/$binary" "$STAGE/usr/bin/$binary"
done
ln -s microwave-ctl "$STAGE/usr/bin/chef-server-ctl"

install -m 0644 packaging/debian/microwave-server.service \
    "$STAGE/usr/lib/systemd/system/microwave-server.service"
install -m 0644 packaging/debian/microwave-server.logrotate \
    "$STAGE/etc/logrotate.d/microwave-server"
install -m 0644 packaging/microwave.toml.example \
    "$STAGE/etc/microwave/microwave.toml"
install -m 0644 README.md LICENSE "$DOC_DIR/"

for script in postinst prerm postrm; do
    install -m 0755 "packaging/debian/$script" "$STAGE/DEBIAN/$script"
done

printf '/etc/microwave/microwave.toml\n/etc/logrotate.d/microwave-server\n' > "$STAGE/DEBIAN/conffiles"

sed -e "s|@VERSION@|$VERSION|g" \
    -e "s|@ARCH@|$DEB_ARCH|g" \
    -e "s|@MAINTAINER@|$DEB_MAINTAINER|g" \
    -e "s|@LIBC_DEP@|$LIBC_DEP|g" \
    packaging/debian/control.in > "$STAGE/DEBIAN/control"

{
    cat packaging/debian/copyright
    echo
    echo "The full license text of this package follows."
    echo
    cat LICENSE
} > "$DOC_DIR/copyright"

(
    cd "$STAGE"
    find usr -type f -print0 | sort -z | xargs -0 md5sum > DEBIAN/md5sums
)

OUTPUT_DIR="$ROOT/${DEB_OUTPUT_DIR:-dist}"
mkdir -p "$OUTPUT_DIR"
DEB="$OUTPUT_DIR/${PKG}_${VERSION}_${DEB_ARCH}.deb"
rm -f "$DEB"
dpkg-deb --root-owner-group --build "$STAGE" "$DEB" >/dev/null

echo "built: $DEB"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$DEB"
fi
