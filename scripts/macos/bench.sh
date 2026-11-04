#!/bin/zsh
# Measures how a game plays in the release app: how long a key takes to
# reach the window, how evenly frames arrive, the frame rate, and the CPU
# it costs. The app runs behind the frontmost app, in a home folder of its
# own with copies of the user's settings and of the game's saves, so
# nothing of the user's changes. Keys are posted to it alone, a tap at a
# time; bench-report.py reads the app's timing trace (trace.rs).
#
# usage: scripts/macos/bench.sh GAME [seconds, default 20] [out-dir]
#   GAME is a ROM, a 3DS game or an RPG Maker game's folder; an RPG Maker
#   game's player and a 3DS game's helper are counted as the player's CPU.
#   The window has to stay partly visible: GPUI stops drawing a window
#   that is covered.
#   KEYS="w s" picks the keys tapped, in turn (default: w and s), and
#   SPEED=normal turns the user's fast-forward off, FRONT=1 leaves the app
#   in front, with the keyboard, and WINDOW="x y width height" places its
#   window (DISPLAY_UUID moves its place to that display). PICTURE (an
#   `[video] picture` name) and RES3D ("auto" or 1 to 8) set the picture
#   and 3D resolution for the run, BIN runs that build of the app instead
#   of this tree's, and CAPTURE=1 keeps window.png of the run's end.
set -uo pipefail
root=${0:A:h:h:h}
tools=$root/target/macos-tools
game=${1:?usage: bench.sh GAME [seconds] [out-dir]}
seconds=${2:-20}
out=${3:-/tmp/ds-emulator-bench}
ax=$tools/ax
mkdir -p $tools
[[ $tools/ax -nt $root/scripts/macos/ax.swift ]] || swiftc -O $root/scripts/macos/ax.swift -o $tools/ax || exit 1
[[ -n ${BIN:-} ]] || cargo build --release -p emu-app --quiet --manifest-path $root/Cargo.toml || exit 1

real="$HOME/Library/Application Support/ds-emulator"
home=$out/home
config="$home/Library/Application Support/ds-emulator"
rm -rf $out
mkdir -p "$config" $out/games
for file in settings.toml bindings.toml; do
  [[ -f "$real/$file" ]] && cp "$real/$file" "$config/"
done
# The 3DS core and the emulated console's files, cloned, so a 3DS game
# neither downloads the core again nor touches the user's console.
[[ -d "$real/3DS" ]] && cp -c -R "$real/3DS" "$config/3DS"
# WINDOW="x y width height" places the window.
if [[ -n ${WINDOW:-} && -f "$config/settings.toml" ]]; then
  read wx wy ww wh <<<$WINDOW
  sed -i '' -e "s/^x = .*/x = $wx.0/" -e "s/^y = .*/y = $wy.0/" \
    -e "s/^width = .*/width = $ww.0/" -e "s/^height = .*/height = $wh.0/" "$config/settings.toml"
fi
# PICTURE and RES3D set View > Picture and the 3D resolution for the run.
[[ -f "$config/settings.toml" ]] && PICTURE=${PICTURE:-} RES3D=${RES3D:-} python3 -I -c '
import os, re, sys
path = sys.argv[1]
text = open(path).read()
for key, value in (("picture", os.environ["PICTURE"] and repr(os.environ["PICTURE"]).replace(chr(39), chr(34))), ("resolution_3d", os.environ["RES3D"])):
    if value:
        text = re.sub(rf"^{key} = .*\n", "", text, flags=re.M)
        if key == "picture":
            text = re.sub(r"^filter = .*\n", "", text, flags=re.M)
        text = text.replace("[video]\n", f"[video]\n{key} = {value}\n", 1)
open(path, "w").write(text)
' "$config/settings.toml"
# DISPLAY_UUID moves the window's place to that display.
[[ -n ${DISPLAY_UUID:-} && -f "$config/settings.toml" ]] &&
  sed -i '' "s/^display = .*/display = \"$DISPLAY_UUID\"/" "$config/settings.toml"
# SPEED=normal turns the user's fast-forward off for the run.
[[ ${SPEED:-} == normal && -f "$config/settings.toml" ]] &&
  sed -i '' 's/^fast_forward_on = true/fast_forward_on = false/' "$config/settings.toml"
# A ROM is copied with its saves and states, so the game picks up where
# the user left it; APFS clones make that instant. A folder is played
# where it is.
if [[ -f $game ]]; then
  for file in "${game:r}".*(N); do cp -c "$file" $out/games/; done
  game=$out/games/${game:t}
fi

front=$(osascript -e 'tell application "System Events" to get unix id of first process whose frontmost is true')
HOME=$home DS_EMULATOR_TRACE=$out/trace.txt ${BIN:-$root/target/release/ds-emulator} $game >$out/app.log 2>&1 &
pid=$!
# The app activates itself on launch; hand the front back right away.
sleep 0.6
[[ -n ${FRONT:-} ]] ||
  osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $front) to true"

# Wait for frames, then let the game settle before measuring.
for _ in {1..300}; do grep -q ' run ' $out/trace.txt 2>/dev/null && break; sleep 0.2; done
sleep 3
# An RPG Maker game's hidden player, or a 3DS game's helper.
player=$(pgrep -f "$home/Library/Application Support/ds-emulator/RPG Maker/.*/Player.app" | head -1)
[[ -n $player ]] || player=$(pgrep -f -- "--3ds-helper .*$home/" | head -1)
cpu() { ps -o time= -p $1 2>/dev/null | awk -F'[:.]' '{ print ($1 * 60 + $2) + ("0." $3) }'; }
started=$(perl -MTime::HiRes=time -e 'printf "%.3f", time')
app_cpu=$(cpu $pid) player_cpu=${player:+$(cpu $player)}
keys=(${=KEYS:-w s})
end=$(( $(date +%s) + seconds ))
n=0
while (( $(date +%s) < end )); do
  $ax $pid key ${keys[$(( n % ${#keys} + 1 ))]} >/dev/null
  n=$((n + 1))
  # Taps land at no fixed point of a frame.
  sleep 0.$(( RANDOM % 300 + 200 ))
done
finished=$(perl -MTime::HiRes=time -e 'printf "%.3f", time')
# CAPTURE=1 keeps a picture of the window as the run ends.
[[ -n ${CAPTURE:-} ]] && screencapture -x -o -l $($ax $pid winid) $out/window.png
app_cpu_end=$(cpu $pid) player_cpu_end=${player:+$(cpu $player)}
kill $pid 2>/dev/null
wait $pid 2>/dev/null
extra=()
[[ -n $player ]] && extra=(--player-cpu $(( player_cpu_end - player_cpu )))
python3 -I $root/scripts/macos/bench-report.py $out/trace.txt \
  --wall $(( finished - started )) --taps $n --app-cpu $(( app_cpu_end - app_cpu )) $extra
