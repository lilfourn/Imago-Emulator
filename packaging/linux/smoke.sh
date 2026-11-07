#!/bin/sh
# Installs a package made by package.sh on a clean system and starts it, as
# a user would: checks that every library it needs is there, then plays a
# game under Xvfb until it has drawn its first frames, screenshots it, and
# closes its window.
#
# usage: packaging/linux/smoke.sh PACKAGE ROM [OUT_DIR]
#
#   PACKAGE  Imago-Emulator-*.deb or Imago-Emulator-*.AppImage
#   ROM      a homebrew test ROM, such as test-roms/armwrestler-ds/
#            armwrestler.nds (scripts/fetch-test-roms.sh); it is copied,
#            as the app writes saves beside it
#   OUT_DIR  the screenshot, logs and results.txt (default: ./smoke-package)
#
# Run it as root in a fresh container of the system to check, which it
# changes: a .deb on Debian or Ubuntu (apt installs it and what it
# depends on), an AppImage on any system with apt or dnf. For an AppImage
# it first installs only what the AppImage takes from the system (see
# package.sh), so a library it should carry and doesn't shows as missing.
# Then it installs the test's own tools: Xvfb, Openbox, xdotool, wmctrl
# and ImageMagick. Openbox stands in for a desktop's window manager:
# without one, GPUI 0.2.2 on X11 draws its window once and never again.
#
# The release workflow runs it on Ubuntu 22.04 and 24.04 for the .deb and
# Fedora for the AppImage. From a Mac:
#
#   docker run --rm -v "$PWD:/w" -w /w ubuntu:24.04 \
#       packaging/linux/smoke.sh target/dist/Imago-Emulator-*.deb \
#       test-roms/armwrestler-ds/armwrestler.nds target/smoke-deb
#
# Closing the window is reported but doesn't fail the run: GPUI 0.2.2
# panics when the app quits from its close callback on X11 (#62).
set -eu

[ "$#" -ge 2 ] || {
    echo "usage: $0 PACKAGE ROM [OUT_DIR]" >&2
    exit 2
}
package="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
rom="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
out="${3:-smoke-package}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
[ -f "$package" ] || {
    echo "$0: no package at $1" >&2
    exit 2
}
[ -f "$rom" ] || {
    echo "$0: no ROM at $2" >&2
    exit 2
}

results="$out/results.txt"
: >"$results"
failed=0
# check NAME pass|fail|note DETAIL
check() {
    [ "$2" = fail ] && failed=1
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$results"
    printf '%-10s %s: %s\n' "$1" "$2" "$3" >&2
}
say() { printf '\n== %s\n' "$*" >&2; }

# shellcheck disable=SC1091 # the system's own
# shellcheck disable=SC1091 # the system's own
. /etc/os-release
system="$PRETTY_NAME"
if command -v apt-get >/dev/null; then
    manager=apt
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    add() { apt-get install -y -qq --no-install-recommends "$@" >/dev/null; }
elif command -v dnf >/dev/null; then
    manager=dnf
    add() { dnf install -y -q --setopt=install_weak_deps=False "$@" >/dev/null; }
else
    echo "$0: needs apt-get or dnf" >&2
    exit 2
fi

case "$package" in
    *.deb)
        [ "$manager" = apt ] || {
            echo "$0: a .deb needs Debian or Ubuntu" >&2
            exit 2
        }
        say "Installing $(basename "$package") on $system"
        if apt-get install -y -qq --no-install-recommends "$package" >"$out/install.log" 2>&1; then
            check install pass "apt installed it and what it depends on"
        else
            check install fail "apt refused it; see install.log"
            cat "$out/install.log" >&2
            exit 1
        fi
        app=/usr/bin/ds-emulator
        binary="$app"
        for path in /usr/share/applications/ds-emulator.desktop \
            /usr/share/metainfo/com.lilfourn.ds-emulator.metainfo.xml \
            /usr/share/icons/hicolor/256x256/apps/ds-emulator.png \
            /usr/share/doc/imago-emulator/copyright \
            /usr/share/imago-emulator/License.txt \
            /usr/share/imago-emulator/Acknowledgements.html \
            /usr/share/imago-emulator/Source/seed_tables.rs; do
            [ -f "$path" ] || check files fail "$path is missing"
        done
        grep -q "^files" "$results" || check files pass "desktop file, metainfo, icons and notices"
        ;;
    *.AppImage)
        say "The AppImage on $system"
        # What the AppImage takes from the system, by each system's names.
        if [ "$manager" = apt ]; then
            add ca-certificates curl libasound2t64 2>/dev/null || add ca-certificates curl libasound2
            add libudev1 libxcb1 libwayland-client0 libvulkan1 mesa-vulkan-drivers
        else
            add curl alsa-lib systemd-libs libxcb libwayland-client vulkan-loader mesa-vulkan-drivers
        fi
        check install pass "the system's sound, udev, xcb, Wayland and Vulkan libraries"
        chmod +x "$package"
        # Its files, to check what it links without mounting it.
        extracted="$(mktemp -d)"
        (cd "$extracted" && "$package" --appimage-extract >/dev/null)
        binary="$extracted/squashfs-root/usr/bin/ds-emulator"
        app="$package"
        for path in AppRun ds-emulator.desktop ds-emulator.png \
            usr/share/imago-emulator/License.txt \
            usr/share/imago-emulator/Acknowledgements.html \
            usr/share/imago-emulator/Source/seed_tables.rs; do
            [ -e "$extracted/squashfs-root/$path" ] || check files fail "$path is missing"
        done
        grep -q "^files" "$results" || check files pass "desktop file, icon and notices"
        ;;
    *)
        echo "$0: PACKAGE must be a .deb or an .AppImage" >&2
        exit 2
        ;;
esac

# Every library resolves, those it opens itself included.
ldd "$binary" >"$out/ldd.txt" 2>&1 || true
if grep -q 'not found' "$out/ldd.txt"; then
    check libraries fail "$(grep 'not found' "$out/ldd.txt" | awk '{ print $1 }' | tr '\n' ' ')missing"
else
    missing=""
    for soname in libvulkan.so.1 libwayland-client.so.0; do
        ldconfig -p | grep -q "$soname" || missing="$missing $soname"
    done
    if [ -n "$missing" ]; then
        check libraries fail "missing:$missing"
    else
        carried="$(grep -c squashfs-root "$out/ldd.txt" || true)"
        check libraries pass "all found; $carried from inside the AppImage"
    fi
fi

say "Starting it under Xvfb"
if [ "$manager" = apt ]; then
    add xvfb xauth openbox xdotool wmctrl x11-utils imagemagick
else
    add xorg-x11-server-Xvfb openbox xdotool wmctrl xdpyinfo ImageMagick
fi
work="$(mktemp -d)"
Xvfb -displayfd 3 -screen 0 1920x1080x24 -nolisten tcp 3>"$work/display" >"$out/xvfb.log" 2>&1 &
xvfb=$!
wm=""
pid=""
# shellcheck disable=SC2329 # run by the trap below
cleanup() {
    [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null
    [ -n "$wm" ] && kill "$wm" 2>/dev/null
    kill "$xvfb" 2>/dev/null
    rm -rf "$work"
}
trap cleanup EXIT INT TERM
for _ in $(seq 100); do
    [ -s "$work/display" ] && break
    sleep 0.1
done
DISPLAY=":$(cat "$work/display")"
export DISPLAY
openbox --sm-disable >"$out/openbox.log" 2>&1 &
wm=$!
for _ in $(seq 100); do
    wmctrl -m >/dev/null 2>&1 && break
    sleep 0.1
done

# A home of its own, with libretro's box-art listings cached and naming
# nothing, so nothing is downloaded; and a copy of the ROM.
home="$work/home"
listings="$home/.cache/ds-emulator/listings"
mkdir -p "$listings" "$work/roms"
for console in "Game Boy" "Game Boy Color" "Game Boy Advance" "Nintendo DS"; do
    for folder in Named_Boxarts Named_Titles; do
        echo none.png >"$listings/Nintendo - $console - $folder.txt"
    done
done
cp "$rom" "$work/roms/"
trace="$out/trace.txt"
# No FUSE in a container: the AppImage runs from its extracted files.
APPIMAGE_EXTRACT_AND_RUN=1 DS_EMULATOR_HOME="$home" DS_EMULATOR_TRACE="$trace" RUST_BACKTRACE=1 \
    "$app" "$work/roms/$(basename "$rom")" >"$out/app.log" 2>&1 &
pid=$!

window=""
for _ in $(seq 120); do
    window="$(xdotool search --onlyvisible --class ds-emulator 2>/dev/null | head -n 1 || true)"
    [ -n "$window" ] && break
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
done
if [ -z "$window" ]; then
    check window fail "no window within 60 s; see app.log"
    tail -20 "$out/app.log" >&2
    exit 1
fi
check window pass "window $window"

# The game's first 60 frames, and more than a few colours in the middle of
# the window. GPUI 0.2.2 on X11 sometimes draws a window once and stops;
# mapping it again starts it (#62), done once after 15 s, and said.
drawn=""
remapped=""
for waited in $(seq 120); do
    sleep 1
    kill -0 "$pid" 2>/dev/null || break
    frames="$(grep -cE ' drawn( |$)' "$trace" 2>/dev/null || true)"
    if [ "$waited" = 15 ] && [ "$(grep -cE ' render( |$)' "$trace" 2>/dev/null || true)" -le 1 ]; then
        xdotool windowunmap --sync "$window" windowmap --sync "$window" 2>/dev/null || true
        remapped="; it drew only after its window was mapped again"
    fi
    [ "${frames:-0}" -ge 60 ] || continue
    import -window "$window" "$out/game.png" 2>/dev/null || continue
    colours="$(convert "$out/game.png" -gravity center -crop 50%x50%+0+0 +repage -format %k info:)"
    [ "$colours" -gt 3 ] || continue
    drawn=1
    break
done
import -window root "$out/screen.png" 2>/dev/null || true
if kill -0 "$pid" 2>/dev/null; then
    check alive pass "running after its first frames"
else
    check alive fail "stopped; see app.log"
    tail -20 "$out/app.log" >&2
fi
if [ -n "$drawn" ]; then
    check frames pass "$frames frames drawn, $colours colours in the picture$remapped"
else
    check frames fail "${frames:-0} frames drawn$remapped"
fi

wmctrl -i -c "$window" 2>/dev/null || true
for _ in $(seq 300); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
done
if kill -0 "$pid" 2>/dev/null; then
    check close note "still running 30 s after its window was closed"
else
    status=0
    wait "$pid" || status=$?
    pid=""
    if [ "$status" = 0 ]; then
        check close pass "exit status 0"
    else
        check close note "exit status $status$(grep -q panicked "$out/app.log" && echo ', a panic (see app.log)')"
    fi
fi

{
    echo "### $(basename "$package") on $system"
    echo
    echo "| Check | Result | Detail |"
    echo "|---|---|---|"
    awk -F '\t' '{ printf "| %s | %s | %s |\n", $1, $2, $3 }' "$results"
    echo
} >"$out/summary.md"
exit "$failed"
