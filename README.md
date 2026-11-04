<img src="assets/icon.svg" width="64" height="64" alt="">

# ds-emulator

A Game Boy, Game Boy Color, Game Boy Advance and Nintendo DS emulator for
macOS, written in Rust with a [GPUI](https://gpui.rs) frontend. The goal is to
play the Pokémon games of generations 1–5 from your own cartridge dumps.
3DS games play too, on Azahar's core, which the app downloads the first
time ([below](#3ds-games)).

**Status: Phase 4 (Nintendo DS).** The DS core direct-boots retail games
with an HLE BIOS. Pokémon White plays from power-on through a new game to
Route 1, and its in-game save survives a restart. It has:
- an ARM946E-S and an ARM7TDMI in lockstep, with IPC;
- both 2D engines and a software 3D engine with anti-aliasing, which can
  draw at up to 8× its resolution;
- 16-channel sound with capture;
- the game card with flash saves;
- the GBA slot, which reads a GBA game and its save for Pal Park;
- the touchscreen, microphone, firmware and RTC;
- the Wi-Fi chip, for local wireless between two DSes: two Pokémon Whites
  trade in the Union Room.
It passes every test in RockPolish's rockwrestler and every page of
armwrestler for the DS.

The Game Boy and Game Boy Color core passes 170 of 181 open-source test
ROMs, including all of Blargg's CPU and DMG sound tests, every Mooneye test
it runs, dmg-acid2 and cgb-acid2. Games made for the original Game Boy play
in the colours a Game Boy Color gives them. The Game Boy Advance core has:
- an ARM7TDMI that passes all 2.25 million SingleStepTests cases;
- video, sound, DMA, timers and an HLE BIOS;
- SRAM, Flash and EEPROM saves;
- the Pokémon real-time clock;
- a link cable.
It passes every jsmolka test ROM it runs, including the save tests, and
every page of armwrestler. It passes 6,968 of the mGBA test suite's 6,998
checks, including every timer, DMA, memory, I/O read and BIOS math one.
Neither core has been checked against commercial games yet; that needs your
own dumps. The app has:
- sound and gamepads;
- save-state slots with thumbnails, rewind and fast-forward;
- link play, including DS local wireless;
- GBC/GBA colour correction, and DS 3D at 2×, 3× or 4× resolution;
- filters: LCD grid, scanlines, Scale2x and Scale3x;
- screenshots, and video recording to QuickTime movies;
- a store of free homebrew: 1,650 Game Boy, Game Boy Color, GBA, DS and
  3DS games, demos, music carts and tools, each added to the library in
  one click;
- ROM hacks, from IPS, UPS, BPS and xdelta patches;
- randomized Pokémon games, made from your own copies of Red to White 2;
- cheats;
- Lua scripts that read and write memory, press buttons and draw over the
  screens;
- RPG Maker games, such as Pokémon Essentials fan games, played through
  mkxp-z in the app's window;
- a debugger: breakpoints and watchpoints on every CPU, disassembly,
  registers, a memory editor, graphics viewers, I/O registers and the
  scheduler's queue.

See the [roadmap](docs/ROADMAP.md).

## Docs

- [PRD](docs/PRD.md): goals, requirements, legal policy, risks, open questions
- [Roadmap](docs/ROADMAP.md): phases and milestones, GB → GBC → GBA → DS
- [Architecture](docs/ARCHITECTURE.md): crates, the core contract, threads, file formats
- [Scripting](docs/scripting.md): the Lua functions scripts can call
- [Design system](docs/design-system.md): the app's layout rules, and the checks that hold it to them
- [Games we can't run](docs/research/games-we-cant-run.md): store games and homebrew for other systems that the cores can't run yet, with leads
- [Fan-made Pokémon games](docs/research/fan-games.md): famous ROM hacks, randomizers and PC fan games, what each needs, and the plan to run them all
- [3DS games](docs/research/3ds.md): what hosting Azahar's core takes, what was measured, and what is left
- [Decisions](docs/decisions/): why Rust and GPUI, Game Boy timing, frame pacing, linked cores, video filters, the GBA slot, DS local wireless, RPG Maker games, ROM hacks, the game store, 3DS games, the randomizer

## Build and run

Needs Rust (the pinned toolchain installs itself through `rustup`) and Xcode.
The separate Metal Toolchain download is not needed.

```sh
cargo run --release -p emu-app -- path/to/game.nds   # or open one with ⌘O
cargo run --release -p emu-app -- download.zip       # a zipped game, unpacked first
cargo run --release -p emu-app -- hack.bps           # a ROM hack, made from the game it names
cargo run --release -p emu-app -- fr.gba hack.ips    # a ROM hack, made from fr.gba
cargo run --release -p emu-app -- red.gb blue.gb     # two Game Boys, linked
cargo run --release -p emu-app -- fr.gba em.gba      # two GBAs, linked
cargo run --release -p emu-app -- w.nds w2.nds       # two DSes in wireless range
cargo run --release -p emu-app -- pt.nds em.gba      # a GBA game in the DS's GBA slot
cargo run --release -p emu-app -- --script scripts/lua/frame-counter.lua game.gba
cargo test --workspace
scripts/macos/check-layout.sh   # the app's layout, in each screen state (needs the test ROMs)
```

A game downloaded as a `.zip`, `.7z` or `.rar` can be opened, dropped on the
window or added to the library as it is. The ROMs inside are unpacked into
`~/Library/Application Support/ds-emulator/Games`, where they and their saves
stay after the download is deleted. Unpacking the same game again reuses it; a
different game with the same file name is numbered, as in `Game (2).gba`.
`.7z` and `.rar` files are unpacked by macOS's own `tar`.

Run a core without a window, for tests and scripting:

```sh
cargo run -p emu-headless -- --frames 600 --dump /tmp/frames path/to/game.gba
# system=gba frames=600 screen=0 width=240 height=160 crc32=…
cargo run -p emu-headless -- --frames 600 --script scripts/lua/input-macro.lua game.gb
# macro=done frame=92
# system=gb frames=600 screen=0 width=160 height=144 crc32=…
cargo run -p emu-headless -- --frames 600 ~/Games/My\ Fan\ Game   # an RPG Maker game's folder
```

Measure a core's speed, and check that a change to it changes nothing the
game can see: `--state` starts from a save state such as a game's `.ss0`
checkpoint, `--scale` draws DS 3D larger, `--time` says how long the frames
took and `--digest` hashes the sound and the save state at the end.

```sh
cargo run --release -p emu-headless -- --frames 600 --state game.ss0 --time --digest game.nds
# system=nds frames=600 screen=0 width=256 height=192 crc32=…
# system=nds frames=600 screen=1 width=256 height=192 crc32=…
# audio_crc32=… state_crc32=…
# elapsed_ms=2236.9 ms_per_frame=3.728 fps=268.2
```

Measure how a game plays in the release app: how long a key takes to reach
the window, how evenly frames reach the display, and the CPU it costs. It
plays behind the frontmost app, with copies of your settings and the
game's saves, and taps keys at it:

```sh
scripts/macos/bench.sh path/to/game.nds 20          # 20 seconds, with your settings
SPEED=normal FRONT=1 scripts/macos/bench.sh ~/Games/My\ Fan\ Game
```

## Controls

| Game | Key | Gamepad | | App | Shortcut |
|---|---|---|---|---|---|
| D-pad | W A S D or arrow keys | D-pad, left stick | | Open ROM | ⌘O |
| A / B | K, Return or left click (off the touchscreen) / J | East / South | | Pause / resume | ⌘P |
| X / Y (DS) | I / U | North / West | | Reset | ⌘R |
| L / R | Q / E | Shoulder buttons | | Save / load state | ⌘S / ⌘L |
| Start / Select | Space / Delete | Start / Select | | Next / previous state slot | ⌘] / ⌘[ |
| Touchscreen | Click or drag on it | Right stick moves a cursor; click a stick to touch | | Fast-forward / slow motion on/off | ⌘F / ⌘⇧F |
| Fast-forward (2×) | Hold Tab | Right trigger | | Stacked / side by side / one screen / hybrid | ⌘1 / ⌘2 / ⌘3 / ⌘4 |
| Slow motion | Hold 1 | | | Swap screens / Quit | ⌘E / ⌘Q |
| Rewind | Hold R | Left trigger | | Rotate clockwise / anticlockwise | ⌘⌥→ / ⌘⌥← |
| Blow into the microphone (DS) | Hold B | | | Screen gap: 0, 16, 48 or 90 px | ⌘G |
| ZL / ZR (3DS) | Z / C | Unbound; bind them in bindings.toml | | Full screen | ⌃⌘F |
| | | | | Switch player (link play) | ⌘T or click a screen |
| | | | | Save screenshot | ⌘⇧S |
| | | | | Start / stop recording | ⌘⇧R |
| | | | | Run / stop a Lua script | ⌘⇧L / ⌘⌥L |
| | | | | Randomize a Pokémon game | ⌘⌥R |
| | | | | Colour correction on/off | ⌘⇧C |
| | | | | Picture: Auto (sharp pixels at your display's resolution), Sharp Pixels, Smooth, Rounded Pixels, LCD Grid, Scanlines | View > Picture |
| | | | | Debugger | ⌘⇧D |
| | | | | Step / step over / next frame (debugger) | ⌘⇧I / ⌘⇧O / ⌘⇧N |
| | | | | 3D resolution (DS, 3DS) | View > 3D Resolution |
| | | | | Original Game Boy games in colour or grey | View > Original Game Boy Games |
| | | | | Computer's microphone on/off (DS) | Emulation > Use Microphone |
| | | | | Fast-forward speed: 2×, 3×, 4× or unlimited | Emulation > Fast-Forward Speed |
| | | | | Show / hide frame rate and speed | ⌘⌥F |

The keys are laid out for playing on a keyboard: the left hand on W A S D
with the shoulders on Q and E and the 3DS's ZL and ZR on Z and C below
them, the right hand on a square of four keys shaped like the DS face
buttons, U and I (Y and X) above J and K (B and A). On a 3DS the D-pad's
keys and the left stick also tilt the circle pad.

Fast-forward runs at twice normal speed. Emulation > Fast-Forward Speed
picks 2×, 3×, 4× or Unlimited, as fast as the computer goes; it is kept
as `fast_forward` under `[speed]` in settings.toml (`"unlimited"`, or a
multiple such as 1.5), beside `slow_motion` (0.5). Settings files from
before 2× was the default left `fast_forward` out for no limit, and now
get 2×. Sound plays only at normal speed, so fast-forward and slow motion
are silent. Rewind records every frame at normal speed and slower, and
about 60 a second when faster, so it steps back through fast-forwarded
play as quickly as it went by.

View > Show FPS (⌘⌥F) puts a small readout in the window's bottom right
corner: how many frames a second the game runs, or steps back through
while rewinding, measured twice a second, and the speed that makes, where
100% is the console's own (59.83 frames a second for the DS, 59.73 for the
Game Boy and GBA) and 200% is fast-forward at 2×. It is kept as `show_fps`
under `[video]` in settings.toml.

The DS screens start stacked with no gap, in a window that opens as large
as fits the display; full screen shows them at the display's full height,
on black. Hybrid shows one screen large beside both screens small, as in
melonDS; Swap Screens chooses the large one. Rotation turns the whole
arrangement for games played with the DS held like a book, and touch
follows it. A 90 px gap matches the DS hinge. The layout, swap, rotation,
gap, 3D resolution and picture are remembered in
`~/Library/Application Support/ds-emulator/settings.toml`.

View > Picture changes how the screens look. **Auto**, the default,
shows every game as it looks, drawn on the GPU at your display's own
resolution, whatever the display and window size: **Sharp Pixels** for
Game Boy, GBA, DS and RPG Maker games, each console pixel a crisp, even
square, and a smooth fit (Apple's MetalFX) for 3DS games, whose screens
are drawn at a higher resolution already. Sharp Pixels never blurs a
pixel or makes one wider than its neighbour, at any size: only the one
display pixel across each edge is mixed. **Smooth** draws any game with
MetalFX. **Rounded Pixels** reshapes pixel art into smooth curves and
lines (OmniScale, from SameBoy), which changes how the art looks; with DS
3D drawn larger it reshapes only the 2D, and wherever the 3D added detail
the larger 3D is drawn smooth instead. **LCD Grid** draws a thin dark line
between pixels, as on the handhelds' screens; **Scanlines** darkens the
bottom of each row, as on a CRT. Both need at least two of your display's
pixels to each console pixel, and show nothing in a smaller window.
Everything applies after colour correction and turns with the screens. On
a Mac without Metal, Scale2x and Scale3x stand in for Rounded Pixels. 3DS
games filter their own textures only with Smooth (Azahar's ScaleForce) or
Rounded Pixels (MMPX). xBRZ is not offered: its code is GPL-3.0,
which the project doesn't take (see the
[legal policy](docs/PRD.md#9-legal-policy)).

View > 3D Resolution draws DS and 3DS 3D larger than its own resolution:
polygon edges, textures and shading come out smooth, and the 2D layers
around them stay pixel for pixel. **Match Display**, the default, draws it
about as large as the screens are shown on your display, up to 4×, and
keeps the speed you asked for first: when the game falls behind, as 4× 3D
does at 3× fast-forward, it draws 3D a step smaller each second until the
game keeps up, and tries the full size again when you change speed or the
window, display or layout changes. Or pick Native (1×), 2×, 3×, 4×, 6× or 8× (`resolution_3d` under
`[video]` in settings.toml: `"auto"` or 1 to 8). The game still sees native 3D wherever it reads
it back, so emulation and save states are the same at every resolution.
States hold only native frames, so the first frame after loading one, and
the frames rewind steps back through, show 3D at the native resolution.

Games made for the original Game Boy play as on a Game Boy Color, in the
colours it gives them: its boot ROM picks palettes by the title of a game
Nintendo published (red for Pokémon Red, blue for Blue), and gives every
other game the same default. View > Original Game Boy Games > In Grey plays
them on an original Game Boy instead, from the next game opened.
`dmg_colors` under `[game_boy]` in settings.toml can also name one of the
twelve palettes a Game Boy Color gives when a button combination is held
as it starts: `"right"`, `"left+a"`, `"down+b"` and so on.

Hold B to blow into the DS microphone, for games that ask you to blow or
shout. Emulation > Use Microphone listens to the computer's microphone
instead. It is off at every launch, and only how loud it is reaches the
game, as noise.

Gamepad buttons are named by position, so A is the right face button as on a
Nintendo controller. Rebind keys and gamepad controls in
`~/Library/Application Support/ds-emulator/bindings.toml`:

```toml
[keys]
a = ["k", "space"]      # several keys per action
rewind = ["`"]

[gamepad]
a = ["South"]           # Xbox-style: A on the bottom button
b = ["East"]
touch = ["RightThumb"]  # touch at the cursor (default: either stick click)

[gamepads.030000005e0400008e02000000007200]   # one controller, by UUID
rewind = ["LeftTrigger"]
```

Each action lists all of its inputs, so listing one moves it, and an empty
list unbinds it. Gamepad controls have gilrs's names: `South`, `East`,
`North` and `West` for the face buttons, `LeftTrigger` and `RightTrigger`
for the shoulder buttons (L1 and R1), `LeftTrigger2` and `RightTrigger2` for
the triggers below them, `Select`, `Start`, `Mode`, `LeftThumb` and
`RightThumb` for the stick clicks, `DPadUp` and so on, and `LeftStickUp`
and so on for the left stick pushed that way.

### Gamepad hotkeys

Hold Select and press another button to run an app command:

| Chord | Does |
|---|---|
| Select + R1 / L1 | Save / load state |
| Select + Start | Pause / resume |
| Select + D-pad right / left | Next / previous state slot |
| Select + D-pad up | Swap screens |
| Select + D-pad down | Switch player (link play) |
| Select + right trigger | Fast-forward on/off |

The game sees neither button of a chord, even after you let go. Select
alone still reaches the game, as a tap when you let go of it, and a button
that isn't a hotkey, pressed while Select is held, reaches the game with
Select held: a game's own Select combinations still work, and the hotkeys
wait until Select comes up. Choose the modifier and the chords in
`[hotkeys]`:

```toml
[hotkeys]
modifier = ["Mode"]       # hold this instead; [] turns hotkeys off
reset = ["West"]          # Mode + West resets
rewind = ["LeftTrigger2"] # held actions last as long as the chord
swap_screens = []         # no chord
```

The commands are `save_state`, `load_state`, `next_slot`, `previous_slot`,
`pause`, `reset`, `toggle_fast_forward`, `toggle_slow_motion`,
`swap_screens`, `switch_player` and `full_screen`; any held action
(`rewind`, `fast_forward`, a button, a turbo button) works too. Hotkeys
apply to every controller.

### Turbo

`turbo_a`, `turbo_b` and so on, for every button, press and release it over
and over while held. None are bound by default:

```toml
[keys]
turbo_a = ["l"]
turbo_b = ["h"]

[gamepad]
turbo_a = ["RightThumb"]  # takes the stick click from touch

[turbo]
frames = 3                # pressed 3 frames, released 3 (the default)
```

Turbo counts emulated frames, not time, so it keeps pace with fast-forward
and slow motion and repeats exactly from run to run. The default makes ten
presses a second at 60 frames a second, slow enough for games that read
their buttons every second or third frame; `frames = 1` alternates every
frame. Every press lasts the full `frames`: letting go mid-press finishes
it, so a quick tap presses once.

### Bindings for one game

A `game.bindings.toml` next to the ROM, in the same format, is laid over
your bindings when the game starts. Whatever it lists wins over your
controller profiles, which win over your `[gamepad]` section. In link
play, the keyboard and gamepads use the bindings of the game they control.

```toml
# pokemon-emerald.bindings.toml: hold the bottom face button to mash A.
[gamepad]
turbo_a = ["South"]
b = ["West"]
```

Battery saves (`game.sav`), save states (`game.ss1` to `game.ss4`),
cheats (`game.cht`) and the game's own bindings (`game.bindings.toml`) live
next to the ROM. A linked pair keeps each game's `.sav`, `.cht` and
bindings, and saves its states next to the first ROM as `game.ls1` to
`game.ls4`. GBA `.sav` files hold the raw save memory, as in mGBA.

## Screenshots and recordings

File > Save Screenshot (⌘⇧S) saves the screens as a PNG next to the ROM,
named like a macOS screenshot: `game 2026-10-07 at 18.58.12.png`. File >
Start / Stop Recording (⌘⇧R) records a QuickTime movie beside it, `game
2026-10-07 at 18.58.12.mov`, which QuickTime Player opens; a red REC in
the window's corner shows while it runs.

Both take the screens as the console drew them: no colour correction,
filter or rotation, a DS's top screen above its touchscreen, and linked
games side by side. They are at the console's resolution, or at the 3D
resolution when DS 3D is upscaled (a screenshot at 2× is 512×768).

A recording holds every frame the game runs and all of its sound,
whatever the speed: fast-forwarded play comes out at normal speed. Time
that doesn't run is cut: pausing, rewinding (the movie jumps from where
rewinding started to where it stopped), and loading a state. The video
is Photo-JPEG at high quality, twice the console's resolution so every
pixel keeps its colour (DS 3D upscaled to 2× or more is recorded at its
own size), with uncompressed 16-bit stereo sound: about 150 MB a minute
for a DS game. The movie keeps the size it started at if you change the
3D resolution while recording. It needs no ffmpeg; to make a smaller
H.264 copy for sharing:

```sh
ffmpeg -i "game 2026-10-07 at 18.58.12.mov" -c:v libx264 -crf 18 \
  -pix_fmt yuv420p -c:a aac game.mp4
```

Quitting or opening another game stops a recording too. A movie is
indexed when it stops, so one cut short by a crash won't play.

## Link play

Open a game, choose File > Connect Second Console…, and pick a second game
for the same system (or the same one with another save, under another
name). Two Game Boys or GBAs run side by side on one cable; on the GBA the
first game is the parent. Two DSes are in local wireless range of each
other, each shown stacked, side by side, each with its own touchscreen.
Keyboard and gamepad control one console at a time: click its screen or
press ⌘T to switch. Game Boy `.sav` files use the same layout as SameBoy,
mGBA and BGB, including the MBC3 clock, so saves move between them.

The DS's local wireless is the console's own Wi-Fi chip, emulated well
enough for Nintendo's multiplayer protocol: beacons, connecting, and the
host's commands answered by its clients in their time slots
([decision 0007](docs/decisions/0007-ds-local-wireless.md)). The two
consoles get different MAC addresses. There is no internet connection
(Nintendo WFC is gone) and no DS Download Play from the DS menu, since the
emulator boots games directly.

## GBA slot

Open a DS game, choose File > Insert GBA Cartridge…, and pick a GBA game.
The DS restarts with it in the GBA slot, where Pal Park reads it, or start
with both: `ds-emulator pt.nds em.gba`. The GBA game keeps its own `.sav`
next to its ROM, so give the two ROMs different names: `pokemon.nds` and
`pokemon.gba` in one folder would share `pokemon.sav`, and the app refuses
to load them. Pal Park has not been tried yet; that needs GBA dumps.

## Library

While no game runs, the window shows the library: the game you played last,
large, with where you left it and Resume, then every game you have added,
by its box art. Type to search; Tab and ⇧Tab show one system at a time.
A click chooses a game, and a double click, its play button or Return
plays it.

Right-click a game, Control-click it or press ⌘I for its menu: when you
last played it and for how long, when it last saved, how many save states
it has and its size, then Play, Show in Finder and Remove from Library.
Removing a game (or ⌘⌫ on the chosen one) leaves its file and saves where
they are; ⌘Z puts it back. Play time counts only while the game runs, not
while it is paused or under the game menu, and is kept in `library.toml`.

Every game has a picture. Box art, or failing that the title screen, comes
from libretro's thumbnails, by the game's No-Intro name or else by its
title among the pictures libretro lists: from thumbnails.libretro.com or,
when that is down, its mirror on GitHub. Pictures are kept in
`~/Library/Caches/ds-emulator` and fetched again if macOS clears it. A game
libretro has no picture of shows the screen where you left it (saved
beside its checkpoint as `game.ss0.png`), or its DS or RPG Maker icon, or a
plate in its system's colour with its title.

## Game store

The store (the Store tab in the library's top bar, File > Game Store or
⌘⇧G) offers free homebrew that its authors share: Game Boy, Game Boy
Color and GBA games from gbdev's [Homebrew Hub](https://hh.gbdev.io), and
DS and 3DS games from [Universal-DB](https://db.universal-team.net). It
never offers commercial games, ports of them, or fan ports that carry
their art or data. A 3DS game comes as homebrew's own `.3dsx` where its
catalog has one, else as a `.cia`, and plays on the core the app
downloads the first time (see [3DS games](#3ds-games)).

Type to search titles, makers and genres. The filters narrow it to one
system (Tab and ⇧Tab step through them) and to games, demos, music or
tools. Each game has a page with its screenshots (← and →), what its
catalog says of it, who made it and its licence. Get, on the page or on a
game's tile, downloads the game and adds it to the library with its first
screenshot as box art; a game the library has shows Play instead. Return
opens a game's page and there gets or plays it; Escape goes back.

Games go into `~/Library/Application Support/ds-emulator/Games/Store`, a
folder for each, where their saves are kept. The catalogs are fetched once
a day (the refresh button fetches them now), and screenshots as they come
into view; both are cached in `~/Library/Caches/ds-emulator/store`.

Only games seen to start in the app are offered. `scripts/store-compat.py`
downloads every game in both catalogs and runs each one headless for 900
frames, 3DS games on a copy of the app's 3DS core. By 2026-11-03, 1,655
of 1,717 had started, 71 of the 89 3DS games among them. Five of those 3DS
games start on an error screen, so 1,650 are offered and the rest are
left out. Games added to the catalogs since then are offered unchecked.
The ones that don't start, with what stops each, five 3DS games that
start on an error screen, and the NES homebrew the app has no core for,
are listed in [games we can't run](docs/research/games-we-cant-run.md), a
backlog for the cores. Rerun it after changing a core:

```sh
python3 -I scripts/store-compat.py            # reruns only what changed
python3 -I scripts/store-compat.py --report   # the verdicts so far
python3 -I scripts/store-compat.py --backlog  # rewrite the backlog
```

The store never offers commercial games: Pokémon and other retail games
play from your own cartridge dumps, and ROM hacks from patches applied to
them.

See [decision 0010](docs/decisions/0010-game-store.md) for the sources and
why they were chosen.

## RPG Maker games

Pokémon fan games made with Pokémon Essentials on RPG Maker XP play in the
window, from a game folder set up for mkxp-z: one holding `Game.ini` and a
mkxp-z player app. Open the folder (or the app in it), drop it on the
window, or add it to the library, which shows the app's icon. The app runs
the game's own player hidden and shows its frames, so the game takes the
app's bindings and gamepad, pause, fast-forward, screenshots and
recordings ([decision 0008](docs/decisions/0008-rpg-maker-games.md)).

The game needs a preload script that reads the buttons the app holds and
makes save states; `emu-mkxp`'s `bridge.rb` describes what it offers. The
game saves in its own menu, to the same file as when it runs on its own.
A save state is the game's own save, made on the spot: anywhere but in a
battle. Loading one restarts the game straight into it.
There is no rewind, link play, cheats, debugger or memory for these games.

## 3DS games

3DS games play in the window on [Azahar](https://azahar-emu.org)'s
libretro core ([decision 0011](docs/decisions/0011-3ds-games.md)). Open
a `.3ds`, `.cci`, `.cxi`, `.3dsx` or `.cia` file (or Azahar's compressed
`.zcci`, `.zcxi` or `.z3dsx`), drop it on the window, add it to the
library, or open an archive holding one. The first time, the app
downloads Azahar's core (GPL-3.0) and MoltenVK (Apache-2.0), about 67 MB,
checks them against the digests it pins, and keeps them in
`~/Library/Application Support/ds-emulator/3DS`; neither ships with the
app. The game runs in a helper process, the app's own executable, and
draws on the GPU through Vulkan on Metal, at the internal resolution View >
3D Resolution sets.

Games must be decrypted dumps of your own: the core refuses an encrypted
one, and the app holds no keys. Azahar's core can't install a title, so a
`.cia` plays as the executable it holds, which the app writes into
`~/Library/Application Support/ds-emulator/Games` the first time; an
encrypted `.cia` is refused. The controls are the app's own (see above),
with the D-pad's keys also tilting the circle pad, ZL and ZR on Z and C,
and the mouse on the bottom screen. Save states work anywhere and the app
keeps where you left off, but there is no rewind, link play, cheats,
debugger or memory viewer, and `.3dsx` homebrew has no save states
(Azahar's core can't load them for a game without a title ID). A game's own saves, its save data and any
extra data (extdata), live on the emulated SD card, in `3DS/Azahar`, and
the app won't pick up where you left off once they've changed since.
Restart > Start Over sets them aside, renaming their folders to end in
`.bak`, so the game begins anew and the old save is kept.

The first time a game draws something new, it waits while the shaders for
it compile; after that, Azahar and macOS both keep them. To keep those
first waits short, the helper shades vertices on the CPU and lets Azahar's
threads compile at once (decision 0011): Pokémon Omega Ruby's longest
freeze in its opening went from 2 s to a quarter of a second. To shade
vertices on the GPU again, start the app with
`DS_EMULATOR_3DS_OPTIONS=citra_use_hw_shader=enabled`.

## ROM hacks

A ROM hack comes as a patch for the game it was made from: `.ips`, `.ups`
or `.bps` for Game Boy and GBA hacks, `.xdelta` for DS hacks. Open one, drop
it on the window or add it to the library, on its own or in the archive it
was downloaded in. The app applies it to your copy of that game and keeps
the result as a game of its own in
`~/Library/Application Support/ds-emulator/Hacks`, named after the patch, as
in `Radical Red.gba`. It has its own saves and its own place in the library,
and the original ROM is left as it is.

UPS and BPS patches name their game by checksum, so the app finds it among
the games in the library, and checks the result. IPS and xdelta patches
don't, so the app asks which game to patch, unless it was opened with the
patch (`ds-emulator fr.gba hack.ips`, or both in one archive). xdelta
checks its result; IPS can't, so an IPS patch applied to the wrong game
makes a broken one. A patch that needs another version of a game (another
revision or region) says so, with the checksum it wants.

Applying a patch again makes the same game and reuses it. A new version of
a hack whose patch has the same name makes a different game, numbered as in
`Radical Red (2).gba`; to play on from the old one's save, copy
`Radical Red.sav` to `Radical Red (2).sav`. xdelta patches made with
xdelta3's old DJW or FGK compression, which it uses only when asked to, are
not read ([decision 0009](docs/decisions/0009-rom-hack-patches.md)).

## Randomizer

File > Randomize Pokémon Game… (⌘⌥R), or Randomize… in a Pokémon game's
menu in the library, makes a randomized copy of the game: the same game with new
wild Pokémon, trainers, starters and more. It runs the [Universal Pokémon Randomizer
FVX](https://github.com/upr-fvx/universal-pokemon-randomizer-fvx), the
successor of the Universal Pokémon Randomizer ZX. The first time, the app
downloads FVX 1.6.2 with its own Java (41 MB, checked against a pinned
SHA-256) into `~/Library/Application Support/ds-emulator/Randomizer`.

It offers your copies of Red, Blue, Yellow, Gold, Silver, Crystal, Ruby,
Sapphire, Emerald, FireRed, LeafGreen, Diamond, Pearl, Platinum, HeartGold,
SoulSilver, Black, White, Black 2 and White 2. Choose what changes:

| Choice | What changes |
|---|---|
| Wild Pokémon | Wild Pokémon only, the same ones in each place every visit |
| Classic | Your starter, wild Pokémon, trainers' teams, gifts and trades, each about as strong as what it replaces |
| Everything | Classic, and every Pokémon's types, abilities and moves, the TMs, the tutors and the items lying around |
| Pasted | Settings from the randomizer's own window or a guide: copy the settings string, then press ⌘V in the window. ZX and FVX strings both work |

Each also turns evolutions by trade into evolutions by level. The game
goes into the hacks folder as `Pokemon - Platinum Version (Random
482913).nds`, with its own saves and its own place in the library, and its
spoiler log beside it (Spoiler Log in the window opens it). Your copy is
left as it is. The six digits are the seed: the same seed and settings in
FVX make the same game. One game is made at a time; closing the window
doesn't stop it, and the main window says when it is done. The 3DS games
are not offered yet, because a randomized copy would share its save with
the game it came from ([decision 0012](docs/decisions/0012-randomizer.md)).

## Cheats

Choose File > Edit Cheats… to open the game's `game.cht` in your text
editor. Save the file and switch back to apply it:

```text
[Infinite money] ards
# Hold L+R.
94000130 FCFF0000
62111938 00000000
B2111938 00000000
20001DE4 000000FF
D2000000 00000000
[Walk through walls] off
02000F04:01
```

Each cheat is a `[name]` line followed by its codes. After the name, `off`
disables it and a tag names the cheat device; without a tag the device is
detected from the codes. Spaces and line breaks inside codes don't
matter, except that Game Genie codes are separated by spaces, line breaks
or `+`. Errors name the line and the device.

| Tag | Device | System |
|---|---|---|
| `raw` | `ADDRESS:VALUE` patches (hex) | all |
| `gs` | GameShark `TTVVAAAA`, including `8x`/`9x` bank codes | Game Boy |
| `gg` | Game Genie `ABC-DEF` or `ABC-DEF-GHI` (ROM patches) | Game Boy |
| `cb` | CodeBreaker / Xploder, encrypted or not | GBA |
| `gsa`, `gsa-raw` | GameShark Advance / Action Replay v1/v2, encrypted or not | GBA |
| `par3`, `par3-raw` | Pro Action Replay v3 / Action Replay MAX, encrypted or not | GBA |
| `ards` | Action Replay DS | DS |

Add a tag when detection guesses wrong or asks for one: 16-digit GBA codes
can be read four ways, and the list says when two fit equally well. A dash
marks Game Boy codes as Game Genie ones. Cheats run once a frame, at
VBlank, so button-activated codes respond at once. ROM patches (Game
Genie, GameShark Advance type `6`, Pro Action Replay `18`–`1E`) apply to
every ROM read, through save states and resets, until the cheat is
switched off or removed. Pro Action Replay `C6`/`C7` codes write GBA I/O
registers, except HALTCNT. Not supported: codes that need the cheat
device's own button, and Action Replay DS types `C4`–`C6`.

## Debugger

View > Debugger (⌘⇧D) opens a window on the running game:

- **Toolbar:** pick the CPU (the DS's ARM9 or ARM7; in link play, the
  player you control), then pause or continue (⌘P), step one instruction
  (⌘⇧I), step over a call (⌘⇧O), or run to the end of the frame (⌘⇧N).
  The steps also work from the game's window while the debugger is open.
- **Disassembly** follows the PC; Go to… shows any address. Click the
  gutter beside an instruction to set a breakpoint there: the emulation
  stops before that instruction, mid-frame.
- **Registers** show what the last step changed in yellow, with the flags
  and mode below.
- **Memory** shows anything the CPU can read without side effects, ROM
  and I/O registers included. Jump to a region or Go to… an address.
  Click a byte and type two hex digits to change it; arrow keys move.
  Edits reach RAM, VRAM, palettes and OAM, never ROM or registers.
- **Panel below:**
  - breakpoints and watchpoints, which stop after an instruction reads or
    writes an address or a range (`start-end`);
  - graphics: tiles from any base address at 4 or 8 bits per pixel,
    background maps, sprites with their attributes, palettes (either DS
    engine);
  - the I/O registers by name;
  - the scheduler's queue.

Watchpoints see the CPU's own loads and stores, the ARM9's TCM included.
They do not see DMA, OAM DMA or HDMA, sound or display capture, or what
the HLE BIOS reads and writes for a system call. Until a breakpoint,
watchpoint or step is set, the game runs exactly as it does without the
debugger. Closing the window removes them.

For instruction traces to diff against other emulators, use the `trace`
examples under Testing against test ROMs.

## Scripting

File > Run Script… (⌘⇧L) runs a Lua 5.4 script alongside the game, and
Stop Script (⌘⌥L) stops it; `--script file.lua` runs one from the command
line, in the app or in `emu-headless`. Scripts read and write memory,
press buttons for the next frame, save and load states, pause, log, and
draw text and boxes over the screens, after every frame or in a loop
around `emu.frameadvance()`:

```lua
emu.on_frame(function()
    gui.text(2, 2, "frame " .. emu.frame())
    if memory.read_u8(0xC000) == 0 then joypad.set({a = true}) end
end)
```

They run sandboxed: no files, programs or clock, limited memory and time,
and the same way on every run. An error stops the script with a notice
naming its line; the game runs on. [docs/scripting.md](docs/scripting.md)
lists every function, and [`scripts/lua/`](scripts/lua/) has examples.

## Testing against test ROMs

```sh
scripts/fetch-test-roms.sh                                   # once
cargo test -p emu-gb --test test_roms -- --nocapture         # pass table
cargo run --release -p emu-gb --example trace -- rom.gb 1000 # CPU trace
```

`crates/emu-gb/tests/test-roms-passing.txt` lists every ROM known to pass;
the test fails if one regresses. Record new passes with `UPDATE_PASSING=1`.
The GBA and DS work the same way, with a passing list per suite next to
each `test_roms.rs`:

```sh
scripts/fetch-mgba-suite.sh                            # the mGBA suite, once
cargo test -p emu-gba --test test_roms -- --nocapture  # jsmolka, armwrestler, mGBA
cargo test -p emu-nds --test test_roms -- --nocapture  # rockwrestler, armwrestler
scripts/fetch-arm-tests.sh                             # ~900 MB, once
cargo test -p emu-arm --release --test single_step     # ARM7TDMI
```

armwrestler and rockwrestler are menus: the harness presses its way to
each page or test and reads the result off the screen with the ROM's own
font. The mGBA suite's passing list holds a pass count per test group, and
a group fails if its count drops. A suite whose ROMs are missing is
skipped. CI fetches everything but the mGBA suite, whose only download is
a moving "latest" build; `fetch-mgba-suite.sh` checks it against a pinned
commit's build.

## Workspace

| Crate | Purpose |
|---|---|
| `emu-core` | The `Core` trait and the plain data that crosses it |
| `emu-sched` | Cycle-timestamped event scheduler |
| `emu-state` | Save-state container format |
| `emu-arm` | ARM7TDMI (GBA, DS ARM7) and ARM946E-S (DS ARM9) interpreters |
| `emu-psg` | The Game Boy family's sound generator, shared by GB and GBA |
| `emu-gba-cart` | The GBA cartridge, shared by the GBA core and the DS GBA slot |
| `emu-gb` | Game Boy and Game Boy Color core |
| `emu-gba` | Game Boy Advance core |
| `emu-nds` | Nintendo DS core |
| `emu-mkxp` | RPG Maker games, played by mkxp-z in a hidden process |
| `emu-3ds` | 3DS games, on Azahar's libretro core in a helper process |
| `emu-cheats` | Cheat list parsing and application |
| `emu-patch` | ROM-hack patches: IPS, UPS, BPS and xdelta |
| `emu-script` | Lua scripts, sandboxed, run between frames |
| `emu-cores` | Picks the core for a ROM |
| `emu-headless` | Windowless runner for test ROMs |
| `emu-app` | The GPUI app (`ds-emulator` binary) |

## Legal

This project contains no Nintendo code, BIOS, firmware or games, and never
will. Dump your own cartridges and system files from hardware you own. Don't
open issues or pull requests that link to ROMs or BIOS files. Games, BIOS
dumps and saves are git-ignored, and CI fails if one is committed. See the
[legal policy](docs/PRD.md#9-legal-policy).

The store downloads homebrew from the catalogs that host it, Homebrew Hub
and Universal-DB, under each game's own terms, which its page shows. The
project hosts no games itself. If you made a game in the store and want it
out, ask its catalog: Homebrew Hub takes requests through its
[database's issues](https://github.com/gbdev/database/issues) or at
hh@gbdev.io.

The Game Boy Color's palettes for original Game Boy games come from
SameBoy's open-source boot ROM, copyright © 2015–2026 Lior Halphon, under
the Expat (MIT) license.

Recordings are encoded with the `jpeg-encoder` crate, which is based in
part on the work of the Independent JPEG Group.

Scripts run on Lua 5.4, copyright © 1994–2026 Lua.org, PUC-Rio, built in
through the `mlua` crate; both are under the MIT license.

3DS games play on Azahar's libretro core (GPL-3.0) and MoltenVK
(Apache-2.0), which the app downloads from their own releases when a 3DS
game first opens; neither is part of this project or its builds. The
libretro API's definitions in `emu-3ds` follow `libretro.h` and
`libretro_vulkan.h`, copyright © 2010–2024 the RetroArch team, under the
MIT license.
