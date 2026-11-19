#!/bin/sh
# Publishes imagoemulator.com: the page, the Worker, and the downloads
# beside them at /downloads/, where the DOWNLOAD_ variables in
# wrangler.jsonc point.
#
# usage: site/publish.sh [PACKAGES]
#
# The Mac's disk image is target/dist/Imago-Emulator-VERSION.dmg, as
# scripts/macos/release.sh leaves it. PACKAGES is a folder holding the
# Windows and Linux packages, as the release workflow's artifacts unpack
# (packaging/README.md); files named Imago-Emulator-VERSION-* anywhere in
# it are published. Nothing is published unless Gatekeeper accepts the disk
# image as notarized, every SHA-256 the page prints is its file's, and
# every file a DOWNLOAD_ variable names is there. Downloads are not
# committed: public/downloads/ is ignored, and keeps every version
# published from this checkout so that old links go on working.
set -eu
cd "$(dirname "$0")"

fail() { echo "error: $*" >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' ../Cargo.toml | head -1)
image=../target/dist/Imago-Emulator-$version.dmg
mkdir -p public/downloads
if [ -f "$image" ]; then
    spctl -a -t open --context context:primary-signature -vv "$image" 2>&1 | grep -q 'source=Notarized Developer ID' ||
        fail "Gatekeeper does not accept $image as notarized"
    cp "$image" public/downloads/
fi
if [ -n "${1:-}" ]; then
    find "$1" -type f -name "Imago-Emulator-$version-*" -exec cp {} public/downloads/ \;
fi

# Every file a variable names is there, and small enough: a static file of
# the Worker's may be 25 MiB at most.
for file in $(sed -n 's|.*"DOWNLOAD_[A-Z0-9_]*": "https://imagoemulator.com/downloads/\([^"]*\)".*|\1|p' wrangler.jsonc); do
    [ -f "public/downloads/$file" ] || fail "wrangler.jsonc names $file, which is not in public/downloads (give its folder to this script)"
    [ "$(stat -f %z "public/downloads/$file")" -lt 26214400 ] || fail "$file is over 25 MiB: host it elsewhere"
done
# Every SHA-256 the page prints is its file's.
sed -n 's|.*<span data-sha256="\([^"]*\)">\([0-9a-f]\{64\}\)</span>.*|\1 \2|p' public/index.html | while read -r file sum; do
    [ "$(shasum -a 256 "public/downloads/$file" | cut -d' ' -f1)" = "$sum" ] ||
        fail "the page prints another SHA-256 than $file has: update public/index.html"
done || exit 1
grep -q "data-sha256=\"Imago-Emulator-$version.dmg\"" public/index.html || fail "the page prints no SHA-256 for this version's disk image"

node --test site.test.mjs >/dev/null || fail "the site's tests fail (node --test site/site.test.mjs)"
wrangler deploy --config wrangler.jsonc
