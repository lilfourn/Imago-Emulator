#!/bin/sh
# Makes the patches in crates/emu-patch/tests/fixtures with the tools hack
# authors use, from the made-up ROMs its tests make: Floating IPS (`flips`,
# https://github.com/Alcaro/Flips) and xdelta3 (`brew install xdelta`), and
# Archipelago's own code for its patches (`uv`, for Python and bsdiff4).
# None is needed to run the tests.
set -eu

roms=$(mktemp -d)
trap 'rm -rf "$roms"' EXIT
EMU_PATCH_FIXTURE_ROMS="$roms" cargo test -q -p emu-patch --test formats -- \
    --ignored --exact write_fixture_roms

out=crates/emu-patch/tests/fixtures
mkdir -p "$out"
source="$roms/source.bin"
target="$roms/target.bin"

flips --create --ips "$source" "$roms/ips-target.bin" "$out/flips.ips"
flips --create --bps-delta "$source" "$target" "$out/flips.bps"
# Linear BPS cannot move bytes, so it gets the smaller change too.
flips --create --bps-linear "$source" "$roms/ips-target.bin" "$out/flips-linear.bps"

xdelta3 -e -f -S none -s "$source" "$target" "$out/xdelta3.xdelta"
xdelta3 -e -f -S lzma -s "$source" "$target" "$out/xdelta3-lzma.xdelta"
xdelta3 -e -f -S none -W 16384 -s "$source" "$target" "$out/xdelta3-windows.xdelta"
xdelta3 -e -f -S lzma -W 16384 -s "$source" "$target" "$out/xdelta3-lzma-windows.xdelta"
xdelta3 -e -f -S djw -s "$source" "$target" "$out/xdelta3-djw.xdelta"

# Archipelago's patches, written and applied again by the file of its that
# does both, as its 0.6.8 release has it.
curl -fsSL -o "$roms/Files.py" \
    https://raw.githubusercontent.com/ArchipelagoMW/Archipelago/0.6.8/worlds/Files.py
uv run --quiet --with bsdiff4 python3 -I scripts/make-archipelago-fixtures.py \
    "$roms/Files.py" "$roms" "$out"
