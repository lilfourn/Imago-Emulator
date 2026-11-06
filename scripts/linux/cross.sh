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
image="ds-emulator-linux:24.04"
cache="${DS_LINUX_CACHE:-$HOME/Library/Caches/ds-emulator-linux}"
sysroot="$cache/sysroot"

if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "building $image" >&2
    docker build -q -t "$image" -f "$here/Dockerfile" "$here" >/dev/null
fi

# The sysroot is the image's own headers and libraries, copied again when
# the image changes.
id="$(docker image inspect -f '{{.Id}}' "$image")"
if [ "$(cat "$sysroot/.image" 2>/dev/null || true)" != "$id" ]; then
    echo "copying the sysroot to $sysroot" >&2
    rm -rf "$sysroot"
    mkdir -p "$sysroot"
    container="$(docker create "$image")"
    docker export "$container" | tar -x -C "$sysroot" usr/include usr/lib usr/share/pkgconfig lib
    docker rm "$container" >/dev/null
    echo "$id" >"$sysroot/.image"
fi

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot/usr/lib/aarch64-linux-gnu/pkgconfig:$sysroot/usr/share/pkgconfig"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="$here/in-container.sh"
export DS_LINUX_IMAGE="$image"

# The glibc suffix is Ubuntu 24.04's, which zig links against.
exec cargo-zigbuild "$cmd" --target aarch64-unknown-linux-gnu.2.39 "$@"
