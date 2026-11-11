#!/bin/zsh
# Checks the app's layout (layout.swift) in each screen state, at the
# default and minimum window sizes, without taking over the screen: the
# release build runs behind the frontmost app, in a home folder of its own
# with a seeded library, and is driven through its menus and keys posted to
# it alone. Leaves its captures and reports in the output directory.
#
# usage: scripts/macos/check-layout.sh [out-dir, default /tmp/ds-emulator-layout]
#   STATES="library menu" narrows the matrix.
set -uo pipefail
root=${0:A:h:h:h}
tools=$root/target/macos-tools
# Where the app and `supporter-key` are built.
bin=$root/target/release
out=${1:-/tmp/ds-emulator-layout}
ax=$tools/ax
mkdir -p $tools $out
for tool in ax layout; do
  [[ $tools/$tool -nt $root/scripts/macos/$tool.swift ]] || swiftc -O $root/scripts/macos/$tool.swift -o $tools/$tool || exit 1
done
cargo build --release -p emu-app -p emu-supporter --quiet --manifest-path $root/Cargo.toml || exit 1

# A home of its own, so the user's library and settings stay as they are.
# A test ROM stands in for a game; the library's games are made up, already
# looked up so nothing is downloaded.
game=$root/test-roms/gba-tests/ppu/hello.gba
[[ -f $game ]] || { echo "needs the test ROMs: scripts/fetch-test-roms.sh"; exit 1; }
home=$out/home
config="$home/Library/Application Support/ds-emulator"
seed() {
  rm -rf $home $out/probe
  mkdir -p "$config" $out/probe
  cat >"$config/settings.toml" <<EOF
[video]
show_fps = true

[window]
x = 120.0
y = 80.0
width = 540.0
height = 800.0
EOF
  # libretro's lists of box art, cached and naming nothing, so no game's
  # title finds one and nothing is downloaded.
  local listings="$home/Library/Caches/ds-emulator/listings" system
  mkdir -p $listings
  for system in "Game Boy" "Game Boy Color" "Game Boy Advance" "Nintendo DS"; do
    for folder in Named_Boxarts Named_Titles; do
      print "none.png" >"$listings/Nintendo - $system - $folder.txt"
    done
  done
  [[ $1 == empty ]] && return
  # The first game was played, so the library leads with it.
  rm -rf /tmp/ds-emulator-layout-games
  mkdir -p /tmp/ds-emulator-layout-games
  touch /tmp/ds-emulator-layout-games/1.ss0
  local n=0 platform name
  for platform name in gba "Golden Sun" gb "Pokémon Red Version" nds "Pokémon White Version" \
      gba "The Legend of Zelda: The Minish Cap" gbc "Pokémon Crystal Version" gba "Advance Wars"; do
    n=$((n + 1))
    cat >>"$config/library.toml" <<EOF
[[game]]
path = "/tmp/ds-emulator-layout-games/$n.$platform"
platform = "$platform"
crc32 = "0000000$n"
title = "$name"
looked_up = true

EOF
  done
}

# The library's Pokémon games, as files holding only the title or game code
# in their header, so the randomize window lists them. No Nintendo data.
seed_pokemon() {
  local games=/tmp/ds-emulator-layout-games file at code
  for file at code in 2.gb 308 'POKEMON RED' 3.nds 12 IRAO 5.gbc 308 PM_CRYSTAL; do
    head -c 512 /dev/zero >$games/$file
    printf %s $code | dd of=$games/$file bs=1 seek=$at conv=notrunc 2>/dev/null
  done
}

# A supporter's key and the look chosen with it, so Settings shows a
# supporter's rows. The key is issued here with the owner's signing key,
# which only the owner's Mac has; without it the state is skipped.
seed_supporter() {
  local key
  key=$($bin/supporter-key issue --name "Layout Check" 2>/dev/null) || return 1
  print -r -- $key >"$config/supporter.key"
  cat >>"$config/settings.toml" <<EOF

[look]
accent = "violet"
app_icon = "night"
EOF
}

# The store's catalogs, as if fetched just now so they are not fetched
# again: made-up games whose screenshots can't be had, so each shows its
# system's plate and nothing is downloaded.
seed_store() {
  local store="$home/Library/Caches/ds-emulator/store" entries=() n=0 platform title
  mkdir -p $store
  for platform title in GB "Tobu Tobu Girl" GBC "Sam Mallard and the Case of the Missing Swan" \
      GBA "Ice Skater" GB "2048" GBC "Into the Blue" GBA "Butano Fighter" GB "Geometrix" \
      GBC "Dangan GB" GBA "Goodboy Galaxy Demo" GB "Petris"; do
    n=$((n + 1))
    entries+=("{\"slug\": \"layout-$n\", \"title\": \"$title\", \"developer\": \"Developer $n\",
      \"platform\": \"$platform\", \"typetag\": \"game\", \"basepath\": \"database-gb\",
      \"firstadded_date\": \"2026-10-0${n:0:1}T00:00:00Z\", \"tags\": [\"Puzzle\"],
      \"description\": \"A made-up game for the layout checks.\",
      \"screenshots\": [\"https://store.invalid/$n.png\"],
      \"files\": [{\"filename\": \"game.${(L)platform}\", \"playable\": true}]}")
  done
  print -r -- "{\"entries\": [${(j:,:)entries}]}" >$store/homebrew-hub.json
  print -r -- '[]' >$store/universal-db.json
}

# State: what to open it with, then the keys and menu items to press there.
# Library, search and empty are the library: seeded, searched for "po" and
# empty; context is the chosen game's menu, opened with ⌘I. Game
# runs a test ROM with the speed readout; menu, cheats and restart are its
# game menu's pages, the first with the key guide; settings is the settings
# window, opened with its shortcut as a menu item can't be pressed behind
# other windows, and supporter is that window with a supporter's key in; store
# and page are the store and a game's page in it; randomize is the randomize
# window, listing the library's Pokémon games.
failed=0
for state in ${=STATES:-empty library search context settings supporter game menu cheats restart store page randomize}; do
  # The main window is the only one; its title names the game.
  rom=() steps=() window=main title=''
  case $state in
    empty) seed empty ;;
    library) seed library ;;
    search) seed library; steps=(key p key o) ;;
    context) seed library; steps=(key cmd-i) ;;
    settings) seed library; steps=(key cmd-,); window=settings title=Settings ;;
    supporter)
      seed library
      seed_supporter || { echo "SKIP  supporter: no signing key to issue a key with"; continue; }
      steps=(key cmd-,); window=settings title=Settings ;;
    game) seed library; rom=($game) ;;
    menu) seed library; rom=($game); steps=(key escape) ;;
    cheats) seed library; rom=($game); steps=(key escape key right key return) ;;
    restart) seed library; rom=($game); steps=(key escape key right key right key return) ;;
    store) seed library; seed_store; steps=(key cmd-shift-g) ;;
    page) seed library; seed_store; steps=(key cmd-shift-g key return) ;;
    randomize) seed library; seed_pokemon; steps=(key cmd-alt-r); window=randomize title=Randomize ;;
  esac
  [[ -f $tools/desktop.pid ]] && kill $(<$tools/desktop.pid) 2>/dev/null && sleep 0.5
  front=$(osascript -e 'tell application "System Events" to get unix id of first process whose frontmost is true')
  HOME=$home DS_EMULATOR_PROBE=$out/probe $bin/ds-emulator $rom >$out/app.log 2>&1 &
  pid=$!
  echo $pid >$tools/desktop.pid
  # The app activates itself on launch; hand the front back right away.
  sleep 0.6
  osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $front) to true"
  for _ in {1..50}; do [[ -s $out/probe/main.tsv ]] && break; sleep 0.2; done
  sleep 1
  for action target in $steps; do
    $ax $pid $action $target >/dev/null
    sleep 0.8
  done
  for _ in {1..20}; do [[ -s $out/probe/$window.tsv ]] && break; sleep 0.2; done
  for size in default minimum; do
    if [[ $size == minimum ]]; then
      # Settings and Randomize are one size. A game's menu is taller than
      # the smallest window.
      [[ $window == settings || $window == randomize || $state == context ]] && continue
      $ax $pid size 256 192 >/dev/null
    fi
    $ax $pid place $title >/dev/null
    sleep 1
    png=$out/$state-$size.png
    # Status 2 means the capture didn't match the window; take it again.
    result=2
    for _ in 1 2 3; do
      if screencapture -x -o -l $($ax $pid winid $title) $png 2>/dev/null; then
        $tools/layout $pid $png $out/probe/$window.tsv $title >$png.txt
        result=$?
        [[ $result != 2 ]] && break
      fi
      sleep 1
    done
    if [[ $result == 2 ]]; then
      failed=1
      echo "SKIP  $state, $size window: couldn't capture the window"
    elif [[ $result == 0 ]]; then
      echo "PASS  $state, $size window: $(grep -c '^PASS' $png.txt) checks"
    else
      failed=1
      echo "FAIL  $state, $size window: $(grep -c '^FAIL' $png.txt) of $(grep -c '^PASS\|^FAIL' $png.txt) checks"
      grep '^FAIL' $png.txt | sed 's/^/        /'
    fi
  done
  kill $pid 2>/dev/null
  sleep 0.5
done
exit $failed
