#!/bin/sh
# Checks that the cores give the same frames, sound and save states on
# every OS and CPU (PRD §7): runs emu-headless --digest on each case in
# digests.txt and compares what it prints with the lines recorded there on
# macOS arm64. A difference means some arithmetic, such as a libm call or a
# float conversion, comes out otherwise on this machine.
#
#   scripts/determinism/check.sh [EMU_HEADLESS]
#   scripts/determinism/check.sh --record [EMU_HEADLESS]
#
# EMU_HEADLESS defaults to target/debug/emu-headless (.exe on Windows),
# which `cargo test --workspace` or `cargo build -p emu-headless` builds.
# The ROMs come from scripts/fetch-test-roms.sh. --record writes this
# machine's results into digests.txt; record on macOS arm64 only, after a
# change to a core that is meant to change its output.
#
# Cases named in DETERMINISM_KNOWN_FAILURES (space-separated ROM paths, as
# in digests.txt) are reported but don't fail the run.
#
# Runs in any POSIX shell, including Git Bash on Windows.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
digests="$here/digests.txt"

record=""
if [ "${1:-}" = --record ]; then
    record=1
    shift
fi
headless="${1:-}"
if [ -z "$headless" ]; then
    headless="$root/target/debug/emu-headless"
    [ -x "$headless" ] || headless="$headless.exe"
fi
[ -x "$headless" ] || {
    echo "$0: no emu-headless at $headless; build it with cargo build -p emu-headless" >&2
    exit 2
}
if [ -n "$record" ] && [ "$(uname -s)-$(uname -m)" != Darwin-arm64 ]; then
    echo "$0: recording on $(uname -s) $(uname -m); the digests are meant to come from macOS arm64" >&2
fi
known=" ${DETERMINISM_KNOWN_FAILURES:-} "

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM
recorded="$work/digests.txt"
results="$work/results.txt"
: >"$results"
failed=0

# expected HEADER: the lines recorded under HEADER in digests.txt.
expected() {
    awk -v header="$1" '
        $0 == header { found = 1; next }
        /^== / { found = 0 }
        found && NF && !/^#/
    ' "$digests"
}

# The comments at the top are kept as they are.
awk '/^== / { exit } { print }' "$digests" >"$recorded"

grep '^== ' "$digests" >"$work/cases"
while IFS= read -r header; do
    # == FRAMES ROM [SCRIPT], split into words.
    # shellcheck disable=SC2086
    set -- $header
    frames="$2"
    rom="$3"
    script="${4:-}"
    if [ ! -f "$root/test-roms/$rom" ]; then
        echo "$0: test-roms/$rom is missing; run scripts/fetch-test-roms.sh" >&2
        exit 2
    fi
    set -- --frames "$frames" --digest
    if [ -n "$script" ]; then
        set -- "$@" --script "$here/$script"
    fi
    started="$(date +%s)"
    if ! "$headless" "$@" "$root/test-roms/$rom" >"$work/out" 2>"$work/err"; then
        echo "emu-headless failed: $(head -n 1 "$work/err")" >"$work/out"
    fi
    seconds=$(($(date +%s) - started))
    {
        echo "$header"
        cat "$work/out"
        echo
    } >>"$recorded"

    [ -n "$record" ] && continue
    expected "$header" >"$work/expected"
    if cmp -s "$work/expected" "$work/out"; then
        status=pass
    else
        status=fail
    fi
    case "$known" in
        *" $rom "*)
            if [ "$status" = pass ]; then
                status="pass, but listed in DETERMINISM_KNOWN_FAILURES: take it off"
            else
                status="known failure"
            fi
            ;;
    esac
    printf '%s\t%s\t%s frames%s, %s s\n' "$rom" "$status" "$frames" \
        "${script:+ with $script}" "$seconds" >>"$results"
    printf '%-48s %s\n' "$rom" "$status" >&2
    if [ "$status" != pass ]; then
        echo "  expected:" >&2
        sed 's/^/    /' "$work/expected" >&2
        echo "  got:" >&2
        sed 's/^/    /' "$work/out" >&2
    fi
done <"$work/cases"

if [ -n "$record" ]; then
    # The last case's blank line is dropped.
    sed '$ { /^$/d; }' "$recorded" >"$digests"
    echo "recorded $(grep -c '^== ' "$digests") cases in $digests" >&2
    exit 0
fi

if grep -q "	fail	" "$results"; then
    failed=1
fi
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "## Determinism ($(uname -s) $(uname -m))"
        echo
        echo "Frame, sound and save-state CRC-32s against those recorded on macOS arm64."
        echo
        echo "| ROM | Result | Run |"
        echo "|---|---|---|"
        awk -F '\t' '{ printf "| %s | %s | %s |\n", $1, $2, $3 }' "$results"
        echo
    } >>"$GITHUB_STEP_SUMMARY"
fi
exit "$failed"
