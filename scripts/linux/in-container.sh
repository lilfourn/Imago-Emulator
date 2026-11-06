#!/bin/sh
# Runs one Linux program, such as a test binary cross.sh built, in the
# Ubuntu container. The checkout, the program's folder and the current
# folder are mounted at the same paths as on the Mac, and cargo's CARGO_*
# and RUST_* variables pass through, so tests find their fixtures.
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

exec docker run --rm -i --init -v "$root:$root" -w "$PWD" "$@"
