#!/bin/sh
# Makes the Linux and Windows packages' icons from assets/icon.svg, the
# app's tile without the macOS grid's margin and shadow (that is
# assets/app-icon.svg, for the Mac):
#
#   packaging/linux/icons/hicolor/SIZExSIZE/apps/ds-emulator.png, and
#   scalable/apps/ds-emulator.svg, the freedesktop icon theme's sizes;
#   assets/app-icon.ico, the Windows exe's icon (crates/emu-app/build.rs),
#   each size drawn from the SVG rather than scaled down from the largest.
#
# The results are committed, so a package build needs none of these tools.
# Run it again after changing the icon.
#
#   packaging/icons.sh
#
# Needs resvg (brew install resvg) and Python 3 with Pillow.
set -eu
cd "$(dirname "$0")/.."

svg=assets/icon.svg
hicolor=packaging/linux/icons/hicolor
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

rm -rf "$hicolor"
for size in 16 24 32 48 64 128 256 512; do
    mkdir -p "$hicolor/${size}x$size/apps"
    resvg -w "$size" -h "$size" "$svg" "$hicolor/${size}x$size/apps/ds-emulator.png"
done
mkdir -p "$hicolor/scalable/apps"
cp "$svg" "$hicolor/scalable/apps/ds-emulator.svg"

# Windows picks the size nearest the one it shows: 16 to 48 at 100% to
# 300% scaling in lists and the taskbar, 256 for large icons.
sizes="16 20 24 32 40 48 64 96 128 256"
for size in $sizes; do
    resvg -w "$size" -h "$size" "$svg" "$scratch/$size.png"
done
# shellcheck disable=SC2086 # one argument per size
python3 - "$scratch" assets/app-icon.ico $sizes <<'EOF'
import sys
from PIL import Image

folder, out, *sizes = sys.argv[1:]
images = [Image.open(f"{folder}/{size}.png").convert("RGBA") for size in sizes]
largest = images[-1]
largest.save(
    out,
    format="ICO",
    sizes=[image.size for image in images],
    append_images=images[:-1],
)
EOF
echo "made $hicolor and assets/app-icon.ico"
