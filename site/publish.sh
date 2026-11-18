#!/bin/sh
# Publishes imagoemulator.com: the page, the Worker, and the Mac's disk image
# beside them at /downloads/, where DOWNLOAD_MAC in wrangler.jsonc points.
#
# usage: site/publish.sh [DISK_IMAGE]
#
# DISK_IMAGE defaults to target/dist/Imago-Emulator-VERSION.dmg, as
# scripts/macos/release.sh leaves it. It is published only if Gatekeeper
# accepts it as notarized and its SHA-256 is the one the page prints. Disk
# images are not committed: public/downloads/ is ignored, and holds every
# version published from this checkout so that old links keep working.
set -eu
cd "$(dirname "$0")"

fail() { echo "error: $*" >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' ../Cargo.toml | head -1)
file=Imago-Emulator-$version.dmg
image=${1:-../target/dist/$file}
[ -f "$image" ] || fail "no disk image at $image (scripts/macos/release.sh makes it)"
[ "$(basename "$image")" = "$file" ] || fail "$image is not $file, this version's disk image"
# A static file of the Worker's may be 25 MiB at most.
[ "$(stat -f %z "$image")" -lt 26214400 ] || fail "$image is over 25 MiB: host it elsewhere and point DOWNLOAD_MAC there"
spctl -a -t open --context context:primary-signature "$image" 2>&1 | grep -q 'source=Notarized Developer ID' ||
    spctl -a -t open --context context:primary-signature -vv "$image" 2>&1 | grep -q 'source=Notarized Developer ID' ||
    fail "Gatekeeper does not accept $image as notarized"
sum=$(shasum -a 256 "$image" | cut -d' ' -f1)
grep -q "<span id=\"sha256\">$sum</span>" public/index.html ||
    fail "the page prints another SHA-256 than $image has ($sum): update public/index.html"

node --test site.test.mjs >/dev/null || fail "the site's tests fail (node --test site/site.test.mjs)"

mkdir -p public/downloads
cp "$image" public/downloads/
wrangler deploy --config wrangler.jsonc
