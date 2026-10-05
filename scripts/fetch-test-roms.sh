#!/bin/sh
# Downloads freely redistributable homebrew test ROMs into test-roms/
# (git-ignored). Never commit them; see docs/PRD.md, section 9.
set -eu

version="v7.0"
url="https://github.com/c-sp/game-boy-test-roms/releases/download/$version/game-boy-test-roms-$version.zip"
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/test-roms/game-boy-test-roms"

if [ -f "$dest/.version" ] && [ "$(cat "$dest/.version")" = "$version" ]; then
    echo "test ROMs $version already in $dest"
    exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$url" -o "$tmp/roms.zip"
rm -rf "$dest"
mkdir -p "$dest"
unzip -q "$tmp/roms.zip" -d "$dest"
echo "$version" > "$dest/.version"
echo "test ROMs $version in $dest"
