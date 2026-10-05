#!/bin/sh
# Downloads freely redistributable homebrew test ROMs into test-roms/
# (git-ignored), each bundle pinned to a version. Never commit them; see
# docs/PRD.md, section 9.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"

# fetch NAME VERSION URL: unzips URL into test-roms/NAME unless VERSION is
# already there. A single top-level folder in the archive is flattened.
fetch() {
    name="$1"
    version="$2"
    url="$3"
    dest="$root/test-roms/$name"
    if [ -f "$dest/.version" ] && [ "$(cat "$dest/.version")" = "$version" ]; then
        echo "$name $version already in $dest"
        return
    fi
    tmp="$(mktemp -d)"
    curl -fsSL "$url" -o "$tmp/roms.zip"
    mkdir -p "$tmp/out"
    unzip -q "$tmp/roms.zip" -d "$tmp/out"
    set -- "$tmp"/out/*
    if [ "$#" -eq 1 ] && [ -d "$1" ]; then
        src="$1"
    else
        src="$tmp/out"
    fi
    rm -rf "$dest"
    mkdir -p "$(dirname "$dest")"
    mv "$src" "$dest"
    echo "$version" > "$dest/.version"
    rm -rf "$tmp"
    echo "$name $version in $dest"
}

# Game Boy and Game Boy Color suites (Blargg, Mooneye, acid2, ...).
fetch game-boy-test-roms v7.0 \
    https://github.com/c-sp/game-boy-test-roms/releases/download/v7.0/game-boy-test-roms-v7.0.zip

# Game Boy Advance CPU, memory, BIOS and PPU tests by jsmolka (MIT).
fetch gba-tests a7113b67e63f83a9b321696ddd7042ccfad6c881 \
    https://codeload.github.com/jsmolka/gba-tests/zip/a7113b67e63f83a9b321696ddd7042ccfad6c881
