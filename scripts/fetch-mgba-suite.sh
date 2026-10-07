#!/bin/sh
# Downloads the mGBA test suite (MIT) into test-roms/mgba-suite/
# (git-ignored). Never commit it; see docs/PRD.md, section 9.
#
# The suite has no releases. Its maintainers publish only the latest build,
# under a URL that changes when they push, so this script checks the
# download against the build of the pinned commit below. When upstream moves
# on, it stops with an error: update the commit and checksum, then re-record
# crates/emu-gba/tests/mgba-suite-passing.txt (UPDATE_PASSING=1). That
# moving URL is why CI does not run this script.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/test-roms/mgba-suite"
url=https://s3.amazonaws.com/mgba/suite-latest.zip
# mgba-emu/suite e6942030d25ffe3ba76c72b73a86da073ec857cc, built 2026-07-09.
commit=e6942030d25ffe3ba76c72b73a86da073ec857cc
sha256=b802bb1a08566456f1820d09f8c5362ede30c838297fd05acdf862f1b1e790a5

if [ -f "$dest/.version" ] && [ "$(cat "$dest/.version")" = "$commit" ]; then
    echo "mgba-suite $commit already in $dest"
    exit 0
fi
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$url" -o "$tmp/suite.zip"
actual="$(shasum -a 256 "$tmp/suite.zip" | cut -d ' ' -f 1)"
if [ "$actual" != "$sha256" ]; then
    echo "error: $url is no longer the build of $commit" >&2
    echo "  expected sha256 $sha256" >&2
    echo "  got      sha256 $actual" >&2
    echo "Pin the new commit and checksum in $0 and re-record the passing list." >&2
    exit 1
fi
mkdir -p "$tmp/out"
unzip -oq "$tmp/suite.zip" -d "$tmp/out"
rm -rf "$dest"
mkdir -p "$(dirname "$dest")"
mv "$tmp/out" "$dest"
echo "$commit" > "$dest/.version"
echo "mgba-suite $commit in $dest"
