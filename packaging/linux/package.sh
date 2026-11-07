#!/bin/sh
# Packages a release build of the app for Linux: a .deb and an AppImage,
# named as the Mac's disk image is (Imago-Emulator-VERSION-linux-ARCH.*).
#
# usage: packaging/linux/package.sh [--notices DIR] BINARY [OUT_DIR]
#
#   BINARY        target/<triple>/release/ds-emulator, built with
#                 cargo build --release --locked -p emu-app
#   OUT_DIR       where the packages go (default: target/dist)
#   --notices DIR the license and notices from packaging/notices.sh for
#                 the binary's target; without it this script runs
#                 notices.sh, which needs cargo-about
#   VERSION       the version (default: the workspace's, from Cargo.toml)
#   FORMATS       which to make (default: "deb appimage")
#   TOOLS_DIR     where the pinned AppImage tools are kept between runs
#                 (default: ~/.cache/imago-emulator-packaging)
#
# Runs on Debian or Ubuntu of the binary's architecture: the oldest
# system the packages are for, since the .deb's dependencies come from
# dpkg-shlibdeps and the AppImage's libraries from this system. Releases
# are built on Ubuntu 22.04. Needs dpkg-dev, binutils, patchelf and curl;
# with desktop-file-utils and appstream installed, it also checks the
# desktop file and the metainfo (the release workflow installs them all).
#
# The AppImage takes glibc, sound (ALSA), udev, X11's xcb, Wayland and
# Vulkan with its driver from the system, as a desktop has them and as
# they must match the system's own. It carries only xkbcommon,
# xkbcommon-x11 and xcb-xkb, which not every Wayland desktop installs.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

notices=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --notices)
            notices="$2"
            shift 2
            ;;
        -h | --help)
            sed -n '2,/^set -eu/p' "$0" | sed '$d; s/^# \{0,1\}//'
            exit 0
            ;;
        *) break ;;
    esac
done
[ "$#" -ge 1 ] || {
    echo "usage: $0 [--notices DIR] BINARY [OUT_DIR]" >&2
    exit 2
}
binary="$1"
out="${2:-$root/target/dist}"
version="${VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -1)}"
formats="${FORMATS:-deb appimage}"
tools="${TOOLS_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/imago-emulator-packaging}"

say() { printf '\n== %s\n' "$*" >&2; }
fail() {
    echo "error: $*" >&2
    exit 1
}

[ -f "$binary" ] || fail "no app at $binary"
for tool in dpkg-deb dpkg-shlibdeps readelf strip patchelf curl sha256sum; do
    command -v "$tool" >/dev/null || fail "$tool is missing (see the top of this script)"
done

# The architecture, from the binary, and the names each format uses.
case "$(readelf -h "$binary" | sed -n 's/^ *Machine: *//p')" in
    *X86-64*) arch=x86_64 deb_arch=amd64 ;;
    *AArch64*) arch=aarch64 deb_arch=arm64 ;;
    *) fail "$binary is neither x86-64 nor arm64" ;;
esac
[ "$(dpkg --print-architecture)" = "$deb_arch" ] ||
    fail "a $arch app is packaged on a $deb_arch system: run this on $deb_arch"
name="Imago-Emulator-$version-linux-$arch"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$out"
out="$(cd "$out" && pwd)"

if [ -z "$notices" ]; then
    case "$arch" in
        x86_64) triple=x86_64-unknown-linux-gnu ;;
        aarch64) triple=aarch64-unknown-linux-gnu ;;
    esac
    notices="$scratch/notices"
    "$root/packaging/notices.sh" "$triple" "$notices"
fi
for file in License.txt Acknowledgements.html Source/seed_tables.rs; do
    [ -f "$notices/$file" ] || fail "no $file in $notices (packaging/notices.sh makes it)"
done

say "Imago Emulator $version for $arch"

# The files both packages install, under /usr. The binary keeps its
# symbols, for crash reports, but not its line tables (147 MB to 29 MB).
usr="$scratch/root/usr"
mkdir -p "$usr/bin" "$usr/share/applications" "$usr/share/metainfo" \
    "$usr/share/doc/imago-emulator"
strip --strip-debug -o "$usr/bin/ds-emulator" "$binary"
chmod 755 "$usr/bin/ds-emulator"
cp "$here/ds-emulator.desktop" "$usr/share/applications/"
# The date of the commit packaged, so the same commit gives the same file.
date="$(git -C "$root" log -1 --format=%cs 2>/dev/null || date -u +%Y-%m-%d)"
sed -e "s/@VERSION@/$version/g" -e "s/@DATE@/$date/g" \
    "$here/com.lilfourn.ds-emulator.metainfo.xml" \
    >"$usr/share/metainfo/com.lilfourn.ds-emulator.metainfo.xml"
cp -R "$here/icons" "$usr/share/"
# The license and notices, in the app's own folder: <exe>/../share/
# imago-emulator in both packages. Not under /usr/share/doc, which
# minimal systems (Ubuntu's container images, for one) leave out but for
# each package's copyright file: that is the license again, and the doc
# folder links to the notices.
legal="$usr/share/imago-emulator"
mkdir -p "$legal/Source"
cp "$notices/License.txt" "$notices/Acknowledgements.html" "$legal/"
cp "$notices/Source/seed_tables.rs" "$legal/Source/"
doc="$usr/share/doc/imago-emulator"
cp "$notices/License.txt" "$doc/copyright"
ln -s ../../imago-emulator/Acknowledgements.html "$doc/Acknowledgements.html"
ln -s ../../imago-emulator/Source "$doc/Source"
# Files and folders as a package installs them, whatever this umask is.
find "$scratch/root" -type d -exec chmod 755 {} +
find "$scratch/root" -type f ! -path '*/bin/*' -exec chmod 644 {} +
# Checked where the tools are installed, as the release workflow does.
if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$usr/share/applications/ds-emulator.desktop"
fi
if command -v appstreamcli >/dev/null; then
    appstreamcli validate --no-net "$usr/share/metainfo/com.lilfourn.ds-emulator.metainfo.xml"
fi

make_deb() {
    say "Making the .deb"
    deb="$scratch/deb"
    cp -R "$scratch/root" "$deb"

    # The libraries the binary links, at the versions this system has,
    # which work on this system and every later one.
    mkdir -p "$scratch/shlibs/debian"
    printf 'Source: imago-emulator\n\nPackage: imago-emulator\nArchitecture: any\n' \
        >"$scratch/shlibs/debian/control"
    shlibs="$(cd "$scratch/shlibs" && dpkg-shlibdeps -O "$deb/usr/bin/ds-emulator" |
        sed -n 's/^shlibs:Depends=//p')"
    [ -n "$shlibs" ] || fail "dpkg-shlibdeps found no dependencies"
    # And what it opens itself: Vulkan and a driver for it, which GPUI
    # draws with; Wayland's client library (on X11 it isn't used); curl,
    # for box art, the store, Guide, the randomizer and the 3DS core.
    depends="$shlibs, libvulkan1, mesa-vulkan-drivers | vulkan-icd, libwayland-client0, curl"
    # The open dialog is the desktop portal's. bsdtar unpacks .rar games
    # (or 7-Zip or unrar, which the app also tries), and xdg-open opens
    # the cheat file and spoiler logs in the desktop's text editor.
    recommends="xdg-desktop-portal, libarchive-tools, xdg-utils"
    suggests="7zip | p7zip-full, unrar"

    mkdir -p "$deb/DEBIAN"
    size="$(du -sk "$deb/usr" | cut -f1)"
    cat >"$deb/DEBIAN/control" <<EOF
Package: imago-emulator
Version: $version
Architecture: $deb_arch
Maintainer: ${MAINTAINER:-Luke Fournier <hello@imagoemulator.com>}
Installed-Size: $size
Depends: $depends
Recommends: $recommends
Suggests: $suggests
Section: games
Priority: optional
Homepage: https://imagoemulator.com
Description: Game Boy, Game Boy Advance, Nintendo DS and 3DS emulator
 Imago Emulator plays Game Boy, Game Boy Color, Game Boy Advance and
 Nintendo DS games from your own cartridge dumps, and free homebrew from
 its built-in store. Nintendo 3DS games play too on x86-64, on Azahar's
 core, which the app downloads the first time one is opened.
 .
 Imago Emulator is not affiliated with, endorsed by or sponsored by
 Nintendo.
EOF
    (cd "$deb" && find usr -type f -exec md5sum {} + | sort -k2) >"$deb/DEBIAN/md5sums"
    chmod 644 "$deb/DEBIAN/control" "$deb/DEBIAN/md5sums"
    dpkg-deb --root-owner-group -Zxz --build "$deb" "$out/$name.deb" >/dev/null
    dpkg-deb --info "$out/$name.deb" | sed -n '/Package:/,$p' | sed '/^ Description/,$d' >&2
}

# fetch NAME URL SHA256: the pinned file NAME in the tools folder,
# downloaded once and checked every time.
fetch() {
    mkdir -p "$tools"
    if [ ! -f "$tools/$1" ] || ! echo "$3  $tools/$1" | sha256sum -c --status; then
        curl -fsSL --retry 3 -o "$tools/$1.part" "$2"
        echo "$3  $tools/$1.part" | sha256sum -c --status ||
            fail "$1 from $2 is not the one pinned"
        mv "$tools/$1.part" "$tools/$1"
    fi
    chmod +x "$tools/$1"
}

make_appimage() {
    say "Making the AppImage"
    # appimagetool 1.9.1 and the type 2 runtime of 2025-11-08, which is
    # static, so the AppImage runs without libfuse2. Both are tagged
    # releases; the SHA-256 values are GitHub's asset digests.
    case "$arch" in
        x86_64)
            tool_sha=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
            runtime_sha=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
            ;;
        aarch64)
            tool_sha=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
            runtime_sha=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
            ;;
    esac
    fetch "appimagetool-1.9.1-$arch.AppImage" \
        "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-$arch.AppImage" \
        "$tool_sha"
    fetch "runtime-20251108-$arch" \
        "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-$arch" \
        "$runtime_sha"

    appdir="$scratch/AppDir"
    cp -R "$scratch/root" "$appdir"
    # The libraries it carries, and their licenses. The binary looks for
    # them beside it first ($ORIGIN), and xkbcommon-x11 for its xcb-xkb.
    lib="$appdir/usr/lib"
    mkdir -p "$lib"
    for soname in libxkbcommon.so.0 libxkbcommon-x11.so.0 libxcb-xkb.so.1; do
        path="$(ldconfig -p | awk -v name="$soname" '$1 == name { print $NF; exit }')"
        [ -n "$path" ] || fail "$soname is not installed (libxkbcommon-x11-dev brings it)"
        cp -L "$path" "$lib/$soname"
        chmod 644 "$lib/$soname"
        package="$(dpkg -S "$(readlink -f "$path")" | head -1 | cut -d: -f1)"
        mkdir -p "$appdir/usr/share/doc/$package"
        cp "/usr/share/doc/$package/copyright" "$appdir/usr/share/doc/$package/"
        # shellcheck disable=SC2016 # $ORIGIN is the loader's, not the shell's
        patchelf --set-rpath '$ORIGIN' "$lib/$soname"
    done
    # shellcheck disable=SC2016
    patchelf --set-rpath '$ORIGIN/../lib' "$appdir/usr/bin/ds-emulator"

    # What the AppImage runtime and appimagetool look for at the top.
    ln -s usr/bin/ds-emulator "$appdir/AppRun"
    cp "$here/ds-emulator.desktop" "$appdir/"
    cp "$here/icons/hicolor/256x256/apps/ds-emulator.png" "$appdir/ds-emulator.png"
    ln -s ds-emulator.png "$appdir/.DirIcon"

    # No FUSE in containers: appimagetool runs from its extracted files.
    # Its update information and signing are left out.
    APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" VERSION="$version" \
        "$tools/appimagetool-1.9.1-$arch.AppImage" --no-appstream \
        --runtime-file "$tools/runtime-20251108-$arch" \
        "$appdir" "$out/$name.AppImage" >"$scratch/appimagetool.log" 2>&1 || {
        cat "$scratch/appimagetool.log" >&2
        fail "appimagetool failed"
    }
    chmod 755 "$out/$name.AppImage"
}

for format in $formats; do
    case "$format" in
        deb) make_deb ;;
        appimage) make_appimage ;;
        *) fail "FORMATS takes deb and appimage, not $format" ;;
    esac
done

say "Done"
cd "$out"
for format in $formats; do
    case "$format" in
        deb) file="$name.deb" ;;
        appimage) file="$name.AppImage" ;;
    esac
    sha256sum "$file"
    ls -lh "$file" >&2
done
