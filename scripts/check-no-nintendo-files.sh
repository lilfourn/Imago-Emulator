#!/bin/sh
# Fails if git tracks anything that looks like a game, BIOS/firmware dump,
# battery save or save state. See docs/PRD.md, section 9.
set -eu

pattern='\.(gb|gbc|gba|nds|dsi|srl|3ds|cci|cxi|cia|3dsx|zcci|zcxi|z3dsx|cdc|sav|rtc|ss[0-9]|ls[0-9]|state|zip|7z)$|(^|/)(bios|firmware)[^/]*\.bin$|(^|/)(aes_keys\.txt|seeddb\.bin)$'

if git ls-files | grep -Ei "$pattern"; then
    echo "error: the files above look like games, BIOS/firmware or saves; they must never be committed" >&2
    exit 1
fi
