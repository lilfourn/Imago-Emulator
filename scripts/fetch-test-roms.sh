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
    # -o: some archives hold names that differ only in case (Makefile and
    # makefile), which collide on macOS.
    unzip -oq "$tmp/roms.zip" -d "$tmp/out"
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

# armwrestler for the GBA, Michael Lane's ARM and Thumb tester, with
# destoer's fixes to the load and store tests.
fetch armwrestler-gba 802e55ad61b421f4bbbc6f74f96aa4e5df4d630f \
    https://codeload.github.com/destoer/armwrestler-gba-fixed/zip/802e55ad61b421f4bbbc6f74f96aa4e5df4d630f

# armwrestler for the DS, with Atem2069's fixes so it runs on hardware.
fetch armwrestler-ds de92073daac1838c0f01339f285e312b4865a4c6 \
    https://codeload.github.com/Atem2069/armwrestler-fixed/zip/de92073daac1838c0f01339f285e312b4865a4c6

# RockPolish's DS tests: ARMv5, IPC, the maths unit, WRAM, VRAM and TCM.
fetch rockwrestler b0067a06cddb9bbc9c46ecf0c60a2d572594e581 \
    https://codeload.github.com/RockPolish/rockwrestler/zip/b0067a06cddb9bbc9c46ecf0c60a2d572594e581
