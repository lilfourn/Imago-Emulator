#!/bin/sh
# Decides which of ci.yml's jobs a change needs, from the files it
# changes, and prints them as KEY=true|false lines for $GITHUB_OUTPUT:
#
#   rust        lint, tests, determinism and the smoke test, every OS
#   site        site/'s tests
#   advisories  cargo-deny against Cargo.lock
#
#   scripts/ci/changes.sh [--full] [PATHS_FILE]
#
# PATHS_FILE lists the changed paths, one per line (git diff --name-only).
# Without one, or with --full, everything runs: pushes to main always run
# everything, as the backstop for anything this misses. A path it doesn't
# know runs everything too; only the paths listed below can skip a job.
set -eu

full=""
if [ "${1:-}" = --full ]; then
    full=1
    shift
fi
paths="${1:-}"

all() {
    echo "rust=true"
    echo "site=true"
    echo "advisories=true"
    exit 0
}
[ -z "$full" ] && [ -n "$paths" ] && [ -f "$paths" ] || all

rust=false
site=false
advisories=false
while IFS= read -r path; do
    case "$path" in
        "") ;;
        # Read by no build or test: the docs, the release packaging and
        # its workflows, and the licence.
        docs/* | packaging/* | LICENSE) ;;
        .github/workflows/release-linux-windows.yml | .github/workflows/windows-release.yml) ;;
        site/*) site=true ;;
        # What cargo-deny reads, which the tests read as well.
        Cargo.lock | Cargo.toml | deny.toml | crates/*/Cargo.toml)
            rust=true
            advisories=true
            ;;
        # CI itself: run every job it describes.
        .github/* | scripts/ci/*) all ;;
        # Markdown at the root (README.md); in a crate it may be included.
        *.md) case "$path" in */*) rust=true ;; esac ;;
        *) rust=true ;;
    esac
done <"$paths"

echo "rust=$rust"
echo "site=$site"
echo "advisories=$advisories"
