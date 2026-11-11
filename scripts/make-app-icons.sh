#!/bin/sh
# Draws the app icons a supporter can choose (Settings > App Icon) from
# assets/app-icon.svg: the same drawing in other colours, so that a change
# to the icon is a change to them all. Needs resvg, which made
# assets/app-icon.png too.
#
#   scripts/make-app-icons.sh
#
# Each line below is an icon's name and its colours: the body and its
# edge, the four blocks (the top screen's, the buttons', the lower
# screen's and the pad's) and the marks on them.
set -eu
cd "$(dirname "$0")/.."
out=assets/app-icons
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Each colour replaced below is in the drawing once, or the drawing changed
# and the icons would come out partly in its colours.
for colour in 'fill="#FFFFFF" stroke="#E7E7EA"' 'fill="#3bb7fd"' 'fill="#fea220"' \
  'fill="#fc6146"' 'fill="#65c162"' '<g fill="#ffffff">'; do
  if [ "$(grep -c -- "$colour" assets/app-icon.svg)" -ne 1 ]; then
    echo "make-app-icons: assets/app-icon.svg no longer has $colour once" >&2
    exit 1
  fi
done

while read -r name body edge top buttons lower pad marks; do
  sed \
    -e "s/fill=\"#FFFFFF\" stroke=\"#E7E7EA\"/fill=\"$body\" stroke=\"$edge\"/" \
    -e "s/fill=\"#3bb7fd\"/fill=\"$top\"/" \
    -e "s/fill=\"#fea220\"/fill=\"$buttons\"/" \
    -e "s/fill=\"#fc6146\"/fill=\"$lower\"/" \
    -e "s/fill=\"#65c162\"/fill=\"$pad\"/" \
    -e "s/<g fill=\"#ffffff\">/<g fill=\"$marks\">/" \
    assets/app-icon.svg >"$work/$name.svg"
  resvg -w 1024 -h 1024 "$work/$name.svg" "$out/$name.png"
  echo "$out/$name.png"
done <<'EOF'
night     #17171A #2C2C31 #3bb7fd #fea220 #fc6146 #65c162 #ffffff
ink       #FFFFFF #E7E7EA #232327 #8E8E98 #4B4B53 #B9B9C0 #ffffff
chalk     #17171A #2C2C31 #F4F4F5 #A1A1AA #D4D4D8 #8A8A94 #17171A
blueprint #0070B5 #005C96 #FFFFFF #9BD3F5 #D6ECFB #74C0EE #0070B5
EOF
