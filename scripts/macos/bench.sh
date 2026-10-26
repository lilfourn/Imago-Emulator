#!/bin/zsh
# Measures how a game plays in the release app: how long a key takes to
# reach the window, how evenly frames arrive, the frame rate, and the CPU
# it costs. The app runs behind the frontmost app, in a home folder of its
# own with copies of the user's settings and of the game's saves, so
# nothing of the user's changes. Keys are posted to it alone, a tap at a
# time; bench-report.py reads the app's timing trace (trace.rs).
#
# usage: scripts/macos/bench.sh GAME [seconds, default 20] [out-dir]
#   GAME is a ROM or an RPG Maker game's folder. The window has to stay
#   partly visible: GPUI stops drawing a window that is covered.
#   KEYS="w s" picks the keys tapped, in turn (default: w and s), and
#   SPEED=normal turns the user's fast-forward off, FRONT=1 leaves the app
#   in front, with the keyboard, and WINDOW="x y width height" places its
#   window.
set -uo pipefail
root=${0:A:h:h:h}
tools=$root/target/macos-tools
game=${1:?usage: bench.sh GAME [seconds] [out-dir]}
seconds=${2:-20}
out=${3:-/tmp/ds-emulator-bench}
ax=$tools/ax
mkdir -p $tools
[[ $tools/ax -nt $root/scripts/macos/ax.swift ]] || swiftc -O $root/scripts/macos/ax.swift -o $tools/ax || exit 1
cargo build --release -p emu-app --quiet --manifest-path $root/Cargo.toml || exit 1

real="$HOME/Library/Application Support/ds-emulator"
home=$out/home
config="$home/Library/Application Support/ds-emulator"
rm -rf $out
mkdir -p "$config" $out/games
for file in settings.toml bindings.toml; do
  [[ -f "$real/$file" ]] && cp "$real/$file" "$config/"
done
# WINDOW="x y width height" places the window.
if [[ -n ${WINDOW:-} && -f "$config/settings.toml" ]]; then
  read wx wy ww wh <<<$WINDOW
  sed -i '' -e "s/^x = .*/x = $wx.0/" -e "s/^y = .*/y = $wy.0/" \
    -e "s/^width = .*/width = $ww.0/" -e "s/^height = .*/height = $wh.0/" "$config/settings.toml"
fi
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
HOME=$home DS_EMULATOR_TRACE=$out/trace.txt $root/target/release/ds-emulator $game >$out/app.log 2>&1 &
pid=$!
# The app activates itself on launch; hand the front back right away.
sleep 0.6
[[ -n ${FRONT:-} ]] ||
  osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $front) to true"

# Wait for frames, then let the game settle before measuring.
for _ in {1..300}; do grep -q ' run ' $out/trace.txt 2>/dev/null && break; sleep 0.2; done
sleep 3
player=$(pgrep -f "$home/Library/Application Support/ds-emulator/RPG Maker/.*/Player.app" | head -1)
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
app_cpu_end=$(cpu $pid) player_cpu_end=${player:+$(cpu $player)}
kill $pid 2>/dev/null
wait $pid 2>/dev/null
extra=()
[[ -n $player ]] && extra=(--player-cpu $(( player_cpu_end - player_cpu )))
python3 -I $root/scripts/macos/bench-report.py $out/trace.txt \
  --wall $(( finished - started )) --taps $n --app-cpu $(( app_cpu_end - app_cpu )) $extra
