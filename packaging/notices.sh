#!/bin/sh
# Writes the license and open-source notices a Linux or Windows package
# ships, into one folder, as the Mac's release.sh does for its app and disk
# image (docs/decisions/0016-public-release.md):
#
#   License.txt            the app's LICENSE
#   Acknowledgements.html  the crates' licenses for TARGET, from
#                          scripts/macos/about.toml and about.hbs
#   Source/seed_tables.rs  the MPL-2.0 file whose source must ship
#
# usage: packaging/notices.sh TARGET OUT_DIR
#
# TARGET is the Rust target the package is for, such as
# x86_64-unknown-linux-gnu or x86_64-pc-windows-msvc: the crates differ per
# system. cargo-about only reads Cargo.lock and the crates' sources, so any
# system can write any target's notices.
#
# Needs cargo-about 0.9 (cargo install cargo-about --version 0.9.3 --locked
# --features cli) and perl.
set -eu

[ "$#" = 2 ] || {
    echo "usage: $0 TARGET OUT_DIR" >&2
    exit 2
}
target="$1"
out="$2"
root="$(cd "$(dirname "$0")/.." && pwd)"
command -v cargo-about >/dev/null || {
    echo "$0: cargo-about is not installed (see the top of this script)" >&2
    exit 1
}

mkdir -p "$out/Source"
html="$out/Acknowledgements.html"
cargo about generate --fail --locked -c "$root/scripts/macos/about.toml" \
    -m "$root/crates/emu-app/Cargo.toml" --target "$target" \
    "$root/scripts/macos/about.hbs" -o "$html"

# The template is the Mac's, which says where the Mac app keeps
# seed_tables.rs. Every package here keeps it in Source beside this file.
perl -0pi -e 's/is in the app at\s+Contents\/Resources\/Source, and on the disk image\./is in the Source folder beside this file./' "$html"
grep -q 'is in the Source folder beside this file' "$html" || {
    echo "$0: about.hbs no longer says where seed_tables.rs is; update this script" >&2
    exit 1
}

cp "$root/LICENSE" "$out/License.txt"
cp "$root/crates/emu-cheats/src/seed_tables.rs" "$out/Source/"
