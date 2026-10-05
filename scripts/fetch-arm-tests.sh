#!/bin/sh
# Downloads the SingleStepTests ARM7TDMI per-instruction tests (MIT) into
# test-roms/arm7tdmi/ (git-ignored). Each file is about 20 MB; all 45 are
# about 900 MB. Pass file names to fetch only some:
#   scripts/fetch-arm-tests.sh arm_data_proc_immediate thumb_add_sub
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/test-roms/arm7tdmi"
base="https://raw.githubusercontent.com/SingleStepTests/ARM7TDMI/main/v1"
mkdir -p "$dest"

if [ "$#" -eq 0 ]; then
    set -- $(gh api repos/SingleStepTests/ARM7TDMI/contents/v1 --jq '.[].name | select(endswith(".json.bin")) | rtrimstr(".json.bin")')
fi
for name in "$@"; do
    file="$dest/$name.json.bin"
    if [ -f "$file" ]; then
        continue
    fi
    echo "fetching $name"
    curl -fsSL "$base/$name.json.bin" -o "$file.tmp"
    mv "$file.tmp" "$file"
done
echo "ARM tests in $dest"
