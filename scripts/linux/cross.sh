#!/bin/sh
# Checks, lints and tests the workspace for Linux from a Mac.
# cargo-zigbuild cross-compiles for arm64 Linux against an Ubuntu 24.04
# sysroot, and test binaries run in an Ubuntu 24.04 container (Docker or
# OrbStack), with the checkout at the same path as on the Mac.
#
#   scripts/linux/cross.sh check  --workspace --all-targets
#   scripts/linux/cross.sh clippy --workspace --all-targets -- -D warnings
#   scripts/linux/cross.sh test   -p emu-gb
#
# DS_LINUX_ARCH=x86_64 builds for x86-64 Linux instead, against the same
# Ubuntu's amd64 sysroot, and runs the tests in an amd64 container, which
# OrbStack runs through Rosetta:
#
#   DS_LINUX_ARCH=x86_64 scripts/linux/cross.sh test -p emu-3ds
#
# Needs `brew install zig pkgconf`, `cargo install --locked cargo-zigbuild`
# and a running Docker. The first run builds the image (Dockerfile here)
# and copies its headers and libraries to ~/Library/Caches.
set -eu

if [ "$#" -lt 1 ]; then
    echo "usage: $0 check|clippy|test|build [cargo arguments]" >&2
    exit 2
fi
cmd="$1"
shift
case "$cmd" in
    build) cmd=zigbuild ;;
    check | clippy | test) ;;
    *)
        echo "$0: unknown command $cmd" >&2
        exit 2
        ;;
esac

here="$(cd "$(dirname "$0")" && pwd)"
cache="${DS_LINUX_CACHE:-$HOME/Library/Caches/ds-emulator-linux}"
# Docker's platform option, given only for x86-64, so the arm64 commands
# stay as they were.
platform=""
case "${DS_LINUX_ARCH:-aarch64}" in
    aarch64 | arm64)
        arch=aarch64
        image="ds-emulator-linux:24.04"
        sysroot="$cache/sysroot"
        ;;
    x86_64 | amd64)
        arch=x86_64
        image="ds-emulator-linux:24.04-amd64"
        sysroot="$cache/sysroot-x86_64"
        platform="--platform=linux/amd64"
        ;;
    *)
        echo "$0: DS_LINUX_ARCH must be aarch64 or x86_64, not $DS_LINUX_ARCH" >&2
        exit 2
        ;;
esac

if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "building $image" >&2
    docker build $platform -q -t "$image" -f "$here/Dockerfile" "$here" >/dev/null
fi

# The sysroot is the image's own headers and libraries, copied again when
# the image changes.
id="$(docker image inspect -f '{{.Id}}' "$image")"
if [ "$(cat "$sysroot/.image" 2>/dev/null || true)" != "$id" ]; then
    echo "copying the sysroot to $sysroot" >&2
    rm -rf "$sysroot"
    mkdir -p "$sysroot"
    container="$(docker create $platform "$image")"
    docker export "$container" | tar -x -C "$sysroot" usr/include usr/lib usr/share/pkgconfig lib
    docker rm "$container" >/dev/null
    echo "$id" >"$sysroot/.image"
fi

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot/usr/lib/$arch-linux-gnu/pkgconfig:$sysroot/usr/share/pkgconfig"
case "$arch" in
    aarch64) export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="$here/in-container.sh" ;;
    x86_64) export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="$here/in-container.sh" ;;
esac
export DS_LINUX_IMAGE="$image"
export DS_LINUX_PLATFORM="$platform"

# The glibc suffix is Ubuntu 24.04's, which zig links against.
exec cargo-zigbuild "$cmd" --target "$arch-unknown-linux-gnu.2.39" "$@"
