#!/bin/sh
# Builds Imago Emulator for the public: the app, signed with Developer ID
# under the hardened runtime and notarized, in a notarized disk image
# (docs/decisions/0016-public-release.md).
#
# usage: scripts/macos/release.sh
#
#   IDENTITY        signing identity (default: the Keychain's first
#                   "Developer ID Application" certificate)
#   NOTARY_PROFILE  notarytool Keychain profile (default: imago-emulator),
#                   made once with
#                     xcrun notarytool store-credentials imago-emulator \
#                       --apple-id YOU@EXAMPLE.COM --team-id TEAMID
#   NOTARIZE=0      sign but don't notarize: a build for this Mac alone
#   BUILD           CFBundleVersion (default: the version), raised for
#                   every build put out under one version
#   OUT             output folder (default: target/dist)
#
# Needs Xcode's command-line tools and cargo-about 0.9
# (cargo install cargo-about --version 0.9.3 --locked --features cli).
set -eu
cd "$(dirname "$0")/../.."

name="Imago Emulator"
# Our oldest macOS. The app links MetalFX (View > Picture's GPU upscaling),
# which came with macOS 13, as a required framework, so on 11 or 12 it
# would not launch. The binary must not claim a newer one (checked below).
minimum_system=13.0
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
build=${BUILD:-$version}
out=${OUT:-target/dist}
target_dir=${CARGO_TARGET_DIR:-target}
profile=${NOTARY_PROFILE:-imago-emulator}
notarize=${NOTARIZE:-1}
identity=${IDENTITY:-$(security find-identity -v -p codesigning |
    sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)}

say() { printf '\n== %s\n' "$*"; }
fail() { echo "error: $*" >&2; exit 1; }

[ -n "$identity" ] || fail "no Developer ID Application certificate in the Keychain (set IDENTITY)"
command -v cargo-about >/dev/null || fail "cargo-about is not installed (see the top of this script)"
if [ "$notarize" = 1 ] && ! xcrun notarytool history --keychain-profile "$profile" >/dev/null 2>&1; then
    fail "no notarytool profile \"$profile\" (see the top of this script), or NOTARIZE=0"
fi
scripts/check-no-nintendo-files.sh
if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
    echo "warning: the working tree has changes that are not committed" >&2
fi

say "Building $name $version ($build)"
cargo build --release --locked -p emu-app
binary="$target_dir/release/ds-emulator"
lipo -archs "$binary" | grep -qx arm64 || fail "expected an arm64 binary"
minos=$(vtool -show-build "$binary" | sed -n 's/^ *minos //p' | head -1)
[ "$(printf '%s\n%s\n' "$minos" "$minimum_system" | sort -V | tail -1)" = "$minimum_system" ] ||
    fail "the binary needs macOS $minos, newer than LSMinimumSystemVersion $minimum_system"

say "Making the app"
rm -rf "$out"
app="$out/$name.app"
resources="$app/Contents/Resources"
mkdir -p "$app/Contents/MacOS" "$resources/Source"
cp "$binary" "$app/Contents/MacOS/ds-emulator"
sed -e "s/@VERSION@/$version/g" -e "s/@BUILD@/$build/g" \
    -e "s/@MINIMUM_SYSTEM@/$minimum_system/g" \
    scripts/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint -s "$app/Contents/Info.plist"

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
iconset="$scratch/AppIcon.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" assets/app-icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" assets/app-icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$resources/AppIcon.icns"

# The open-source notices: in full as HTML, and as the About panel's
# credits, which AppKit reads from Credits.rtf (RTF opens at once; HTML
# this long takes WebKit a while).
cargo about generate --fail -c scripts/macos/about.toml -m crates/emu-app/Cargo.toml \
    scripts/macos/about.hbs -o "$resources/Acknowledgements.html"
textutil -convert rtf -output "$resources/Credits.rtf" "$resources/Acknowledgements.html"
cp LICENSE "$resources/License.txt"
# MPL-2.0 asks that the source of its files be available with the app.
cp crates/emu-cheats/src/seed_tables.rs "$resources/Source/"

say "Signing with $identity"
codesign --force --timestamp --options runtime \
    --entitlements scripts/macos/entitlements.plist --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"

notarize() {
    say "Notarizing $(basename "$1")"
    xcrun notarytool submit "$1" --keychain-profile "$profile" --wait --timeout 1h |
        tee "$scratch/notary.log"
    grep -q 'status: Accepted' "$scratch/notary.log" || {
        id=$(sed -n 's/^ *id: //p' "$scratch/notary.log" | head -1)
        [ -n "$id" ] && xcrun notarytool log "$id" --keychain-profile "$profile" >&2
        fail "notarization of $1 was not accepted"
    }
}

if [ "$notarize" = 1 ]; then
    ditto -c -k --keepParent "$app" "$scratch/app.zip"
    notarize "$scratch/app.zip"
    xcrun stapler staple "$app"
fi

say "Making the disk image"
dmg="$out/Imago-Emulator-$version.dmg"
stage="$scratch/stage"
mkdir -p "$stage/Open Source"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
cp LICENSE "$stage/License.txt"
cp "$resources/Acknowledgements.html" "$resources/Source/seed_tables.rs" "$stage/Open Source/"
hdiutil create -quiet -volname "$name" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"
codesign --force --timestamp --sign "$identity" "$dmg"

if [ "$notarize" = 1 ]; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
    say "Gatekeeper"
    spctl --assess --type execute --verbose=2 "$app"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
fi

say "Done"
shasum -a 256 "$dmg"
ls -lh "$dmg"
