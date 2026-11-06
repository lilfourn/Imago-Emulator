#!/bin/sh
# Runs one Linux program, such as a test binary cross.sh built, in the
# Ubuntu container. The checkout, the program's folder and the current
# folder are mounted at the same paths as on the Mac, and cargo's CARGO_*
# and RUST_* variables pass through, so tests find their fixtures.
# DS_LINUX_PLATFORM (cross.sh sets it for x86-64) is Docker's --platform
# option for the image, such as --platform=linux/amd64.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
image="${DS_LINUX_IMAGE:-ds-emulator-linux:24.04}"
program="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
shift

# Docker's options go in front of the image; the program and its
# arguments follow it.
set -- "$image" "$program" "$@"
# test-roms/ may be a link to another checkout's.
roms=""
if [ -L "$root/test-roms" ]; then
    roms="$(cd "$root/test-roms" && pwd -P)"
fi
for dir in "$(dirname "$program")" "$PWD" ${CARGO_TARGET_DIR:+"$CARGO_TARGET_DIR"} ${roms:+"$roms"}; do
    case "$dir" in
        # The checkout is mounted below; the container's own folders stay.
        "$root" | "$root"/* | /usr | /usr/* | /bin | /bin/* | /lib | /lib/* | /etc | /etc/*) ;;
        *) set -- -v "$dir:$dir" "$@" ;;
    esac
done
for name in $(env | grep -E '^(CARGO|RUST)[A-Za-z0-9_]*=' | cut -d= -f1); do
    set -- -e "$name" "$@"
done

# Cargo may know the checkout by its real path (/private/tmp for /tmp).
real="$(cd "$root" && pwd -P)"
if [ "$real" != "$root" ]; then
    set -- -v "$real:$real" "$@"
fi

# Empty for the arm64 image, so the command stays as it was.
platform="${DS_LINUX_PLATFORM:-}"
exec docker run --rm -i --init $platform -v "$root:$root" -w "$PWD" "$@"
