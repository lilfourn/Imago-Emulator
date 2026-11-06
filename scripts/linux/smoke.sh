#!/bin/sh
# Starts the app on Linux under Xvfb and checks it the way a person would
# at a glance: it opens a game from the command line and shows its frames,
# it opens to the library without one, it lives without a sound device,
# and it quits when its window is closed. Screenshots, logs, the layout
# probe and the timing trace of each run go in the output folder, with a
# summary.md.
#
#   scripts/linux/smoke.sh [--bin PATH] [OUT_DIR]
#
# On Linux it runs PATH (by default target/debug/ds-emulator), which must
# already be built; it needs `scripts/linux/install-build-deps.sh --smoke`
# and `scripts/fetch-test-roms.sh`. On a Mac it cross-builds the arm64 app
# with cross.sh and runs itself in the Linux container, rebuilding the
# container's image first if this folder's Dockerfile changed; cross.sh
# then copies its sysroot again. DS_LINUX_ARCH=x86_64 does the same for
# x86-64 in an amd64 container, where cross.sh supports it.
#
# Each check is named run.check, such as game.quit. Those listed in
# SMOKE_KNOWN_FAILURES (space-separated) are reported but don't fail the
# run, so CI can gate on the rest; one that passes says so, to be taken off
# the list.
#
# The app runs in a home of its own (DS_EMULATOR_HOME) on a copy of a test
# ROM, since saves are written beside the ROM. Openbox stands in for a
# desktop's window manager: without one, GPUI 0.2.2 on X11 draws its
# window once at most and never starts redrawing, and closing the window
# through it is what a user's close button does.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

bin=""
out=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --bin)
            bin="$2"
            shift 2
            ;;
        # How the Mac hands SMOKE_KNOWN_FAILURES to the container.
        --known)
            SMOKE_KNOWN_FAILURES="$2"
            shift 2
            ;;
        -h | --help)
            sed -n '2,/^set -eu/p' "$0" | sed '$d; s/^# \{0,1\}//'
            exit 0
            ;;
        *)
            out="$1"
            shift
            ;;
    esac
done
out="${out:-$root/target/smoke}"

if [ "$(uname -s)" = Darwin ]; then
    # The same images, and Docker platform option, as cross.sh's.
    case "${DS_LINUX_ARCH:-aarch64}" in
        aarch64 | arm64)
            arch=aarch64
            image="${DS_LINUX_IMAGE:-ds-emulator-linux:24.04}"
            platform=""
            ;;
        x86_64 | amd64)
            arch=x86_64
            image="${DS_LINUX_IMAGE:-ds-emulator-linux:24.04-amd64}"
            platform="--platform=linux/amd64"
            ;;
        *)
            echo "$0: DS_LINUX_ARCH must be aarch64 or x86_64, not $DS_LINUX_ARCH" >&2
            exit 2
            ;;
    esac
    echo "building $image" >&2
    # shellcheck disable=SC2086 # no option at all for arm64
    docker build $platform -q -t "$image" -f "$here/Dockerfile" "$here" >/dev/null
    if [ -z "$bin" ]; then
        DS_LINUX_ARCH="$arch" "$here/cross.sh" build -p emu-app
        bin="$root/target/$arch-unknown-linux-gnu/debug/ds-emulator"
    fi
    mkdir -p "$out"
    out="$(cd "$out" && pwd)"
    bin="$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
    # in-container.sh mounts the current folder, so the output lands here,
    # and CARGO_TARGET_DIR, which brings in an app from outside the
    # checkout.
    cd "$out"
    CARGO_TARGET_DIR="$(dirname "$bin")" DS_LINUX_IMAGE="$image" DS_LINUX_PLATFORM="$platform" \
        exec "$here/in-container.sh" "$here/smoke.sh" --bin "$bin" \
        --known "${SMOKE_KNOWN_FAILURES:-}" "$out"
fi

bin="${bin:-$root/target/debug/ds-emulator}"
rom="$root/test-roms/armwrestler-ds/armwrestler.nds"
known=" ${SMOKE_KNOWN_FAILURES:-} "

for tool in Xvfb openbox import xdotool wmctrl xdpyinfo; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "$0: $tool is missing; run scripts/linux/install-build-deps.sh --smoke" >&2
        exit 2
    fi
done
[ -x "$bin" ] || {
    echo "$0: no app at $bin; build it with cargo build -p emu-app" >&2
    exit 2
}
[ -f "$rom" ] || {
    echo "$0: no test ROM at $rom; run scripts/fetch-test-roms.sh" >&2
    exit 2
}

# Only what an earlier run left is cleared from the output folder.
mkdir -p "$out"
out="$(cd "$out" && pwd)"
rm -rf "$out"/game* "$out"/library* "$out"/xvfb.log "$out"/openbox.log \
    "$out"/results.txt "$out"/summary.md
work="$(mktemp -d)"
results="$out/results.txt"
: >"$results"
failed=0

# A display of its own, the first free one, and a window manager on it.
Xvfb -displayfd 3 -screen 0 1920x1080x24 -nolisten tcp 3>"$work/display" >"$out/xvfb.log" 2>&1 &
xvfb=$!
wm=""
# shellcheck disable=SC2329 # run by the trap below
cleanup() {
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

# check NAME STATUS DETAIL: records one check's result.
check() {
    c_name="$1"
    c_status="$2"
    case "$known" in
        *" $c_name "*)
            if [ "$c_status" = pass ]; then
                c_status="pass, but listed in SMOKE_KNOWN_FAILURES: take it off"
            else
                c_status="known failure"
            fi
            ;;
        *)
            [ "$c_status" = pass ] || failed=1
            ;;
    esac
    printf '%s\t%s\t%s\n' "$c_name" "$c_status" "$3" >>"$results"
    printf '%-16s %s: %s\n' "$c_name" "$c_status" "$3" >&2
}

# colours FILE [CROP]: how many colours the image has, in CROP (an
# ImageMagick geometry, centred) if given.
colours() {
    if [ -n "${2:-}" ]; then
        convert "$1" -gravity center -crop "$2+0+0" +repage -format %k info:
    else
        convert "$1" -format %k info:
    fi
}

# count WORD FILE: how many trace lines are WORD events.
count() {
    grep -cE " $1( |\$)" "$2" 2>/dev/null || true
}

# A home with libretro's box-art listings cached and naming nothing, so no
# title finds a picture and nothing is downloaded.
home="$work/home"
listings="$home/.cache/ds-emulator/listings"
mkdir -p "$listings" "$work/roms"
for system in "Game Boy" "Game Boy Color" "Game Boy Advance" "Nintendo DS"; do
    for folder in Named_Boxarts Named_Titles; do
        echo none.png >"$listings/Nintendo - $system - $folder.txt"
    done
done
cp "$rom" "$work/roms/"

# run NAME [ROM]: starts the app, waits for its window and for it to draw,
# screenshots it, then closes the window and checks the app quits.
run() {
    name="$1"
    shift
    trace="$out/$name-trace.txt"
    mkdir -p "$out/$name-probe"
    # VK_LOADER_DEBUG=driver names the Vulkan devices in the log.
    DS_EMULATOR_HOME="$home" DS_EMULATOR_PROBE="$out/$name-probe" \
        DS_EMULATOR_TRACE="$trace" RUST_BACKTRACE=1 VK_LOADER_DEBUG=driver \
        "$bin" "$@" >"$out/$name.log" 2>&1 &
    pid=$!

    window=""
    for _ in $(seq 120); do
        window="$(xdotool search --onlyvisible --class ds-emulator 2>/dev/null | head -n 1 || true)"
        [ -n "$window" ] && break
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.5
    done
    if [ -z "$window" ]; then
        check "$name.window" fail "no window within 60 s; see $name.log"
        import -window root "$out/$name.png" 2>/dev/null || true
        kill -9 "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
        return
    fi
    depth="$(xwininfo -id "$window" 2>/dev/null | awk '/Depth:/ { print $2 }')"
    if [ "$depth" = 32 ]; then
        argb=1
    fi
    check "$name.window" pass "window $window, $depth-bit"

    # Drawn once the window shows more than one colour, and for a game,
    # once it has shown its first 60 frames. GPUI 0.2.2 on X11 sometimes
    # draws a window once and never starts redrawing it, even under a window
    # manager; mapping the window again starts it. That is done once, after
    # 15 s with no redraw, and said in the result, so the other checks still
    # tell something.
    drawn=""
    remapped=""
    waited=0
    while [ "$waited" -lt 60 ]; do
        sleep 1
        waited=$((waited + 1))
        kill -0 "$pid" 2>/dev/null || break
        if [ "$waited" = 15 ] && [ "$(count render "$trace")" -le 1 ]; then
            xdotool windowunmap --sync "$window" windowmap --sync "$window" 2>/dev/null || true
            remapped="; it drew only after its window was mapped again"
        fi
        import -window "$window" "$out/$name.png" 2>/dev/null || continue
        [ "$(colours "$out/$name.png")" -gt 1 ] || continue
        if [ "$#" -gt 0 ] && [ "$(count drawn "$trace")" -lt 60 ]; then
            continue
        fi
        drawn=1
        break
    done
    import -window "$window" "$out/$name.png" 2>/dev/null || true
    import -window root "$out/$name-screen.png" 2>/dev/null || true

    if kill -0 "$pid" 2>/dev/null; then
        check "$name.alive" pass "running after its first frames"
    else
        check "$name.alive" fail "stopped before drawing; see $name.log"
    fi
    shown="$(colours "$out/$name.png" 2>/dev/null || echo 0)"
    if [ "$#" -gt 0 ]; then
        # The middle of the window is the game's picture.
        picture="$(colours "$out/$name.png" 50%x50% 2>/dev/null || echo 0)"
        frames="$(count drawn "$trace") frames drawn, $(count run "$trace") run"
        if [ -n "$drawn" ] && [ "$picture" -gt 3 ]; then
            check "$name.frame" pass "$picture colours in the picture; $frames$remapped"
        else
            check "$name.frame" fail "$picture colours in the picture; $frames$remapped"
        fi
    elif [ -n "$drawn" ] && [ "$shown" -gt 16 ]; then
        check "$name.drawn" pass "$shown colours$remapped"
    else
        check "$name.drawn" fail "$shown colours$remapped"
    fi

    # Closed as a user's close button would, through the window manager.
    wmctrl -i -c "$window" 2>/dev/null || true
    for _ in $(seq 300); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
        import -window root "$out/$name-after-close.png" 2>/dev/null || true
        kill -9 "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
        check "$name.quit" fail "still running 30 s after its window was closed"
        return
    fi
    status=0
    wait "$pid" || status=$?
    if [ "$status" = 0 ]; then
        check "$name.quit" pass "exit status 0"
    elif grep -q panicked "$out/$name.log"; then
        # The panic's place, from the crate on, and its message.
        panic="$(grep -m 1 -A 1 panicked "$out/$name.log" | tr '\n' ' ' |
            sed 's#^.*panicked at \(.*/\)\{0,1\}\([^/]*/src/\)#\2#; s/ *$//')"
        check "$name.quit" fail "exit status $status, panicked at $panic"
    else
        check "$name.quit" fail "exit status $status; see $name.log"
    fi
}

# What the app ran on, for the summary.
system="$(sed -n 's/^PRETTY_NAME="\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' /etc/os-release 2>/dev/null)"
system="${system:-$(uname -s)}"
mesa="$(dpkg-query -W -f '${Version}' mesa-vulkan-drivers 2>/dev/null || true)"
xserver="$(dpkg-query -W -f '${Version}' xvfb 2>/dev/null || true)"
argb=""

# The game first, so the library then lists it.
run game "$work/roms/$(basename "$rom")"
run library

device="$(grep -h -m 1 -oE '\[0\] [^ ].*' "$out"/game.log "$out"/library.log 2>/dev/null |
    head -n 1 | sed 's/^\[0\] //; s/ *$//')"
{
    echo "## Linux smoke ($system, $(uname -m))"
    echo
    echo "Mesa ${mesa:-unknown}, Xvfb ${xserver:-unknown}, Openbox; first Vulkan device: ${device:-unknown}."
    echo
    echo "| Check | Result | Detail |"
    echo "|---|---|---|"
    awk -F '\t' '{ printf "| %s | %s | %s |\n", $1, $2, $3 }' "$results"
    echo
    if grep -q 'cannot find card\|Unknown PCM' "$out"/*.log 2>/dev/null; then
        echo "No sound device here (ALSA found no card); the app ran on without one."
        echo
    fi
    # Mesa's software Vulkan before 24.0.2 sends each frame to an X server
    # without DRI3, such as Xvfb, as a depth-24 image, which a 32-bit
    # window refuses; GPUI 0.2.2 always takes a 32-bit visual when there is
    # one. Hardware drivers present another way.
    if [ -n "$argb" ] && [ -n "$mesa" ] && grep -q '	fail\|known failure' "$results" &&
        dpkg --compare-versions "$mesa" lt 24.0.2 2>/dev/null; then
        echo "Mesa $mesa is older than 24.0.2: its software Vulkan (lavapipe) can't show frames in a"
        echo "32-bit window on an X server without DRI3, such as Xvfb. It sends depth-24 images, which"
        echo "the server refuses, so the window stays black while the app draws. GPUI 0.2.2 always takes"
        echo "a 32-bit visual when the server offers one. Hardware drivers aren't affected."
    fi
} >"$out/summary.md"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$out/summary.md" >>"$GITHUB_STEP_SUMMARY"
fi
exit "$failed"
