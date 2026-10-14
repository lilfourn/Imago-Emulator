<img src="assets/icon.svg" width="64" height="64" alt="">

# ds-emulator

A Game Boy, Game Boy Color, Game Boy Advance and Nintendo DS emulator for
macOS, written in Rust with a [GPUI](https://gpui.rs) frontend. The goal is to
play the Pokémon games of generations 1–5 from your own cartridge dumps.

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
- cheats;
- Lua scripts that read and write memory, press buttons and draw over the
  screens;
- a debugger: breakpoints and watchpoints on every CPU, disassembly,
  registers, a memory editor, graphics viewers, I/O registers and the
  scheduler's queue.

See the [roadmap](docs/ROADMAP.md).

## Docs

- [PRD](docs/PRD.md): goals, requirements, legal policy, risks, open questions
- [Roadmap](docs/ROADMAP.md): phases and milestones, GB → GBC → GBA → DS
- [Architecture](docs/ARCHITECTURE.md): crates, the core contract, threads, file formats
- [Scripting](docs/scripting.md): the Lua functions scripts can call
- [Decisions](docs/decisions/): why Rust and GPUI, Game Boy timing, frame pacing, linked cores, video filters, the GBA slot

## Build and run

Needs Rust (the pinned toolchain installs itself through `rustup`) and Xcode.
The separate Metal Toolchain download is not needed.

```sh
cargo run --release -p emu-app -- path/to/game.nds   # or open one with ⌘O
cargo run --release -p emu-app -- download.zip       # a zipped game, unpacked first
cargo run --release -p emu-app -- red.gb blue.gb     # two Game Boys, linked
cargo run --release -p emu-app -- fr.gba em.gba      # two GBAs, linked
cargo run --release -p emu-app -- w.nds w2.nds       # two DSes in wireless range
cargo run --release -p emu-app -- pt.nds em.gba      # a GBA game in the DS's GBA slot
cargo run --release -p emu-app -- --script scripts/lua/frame-counter.lua game.gba
cargo test --workspace
```

A game downloaded as a `.zip` can be opened, dropped on the window or added
to the library as it is. The ROMs inside are unpacked into
`~/Library/Application Support/ds-emulator/Games`, where they and their saves
stay after the download is deleted. Unpacking the same game again reuses it; a
different game with the same file name is numbered, as in `Game (2).gba`.

Run a core without a window, for tests and scripting:

```sh
cargo run -p emu-headless -- --frames 600 --dump /tmp/frames path/to/game.gba
# system=gba frames=600 screen=0 width=240 height=160 crc32=…
cargo run -p emu-headless -- --frames 600 --script scripts/lua/input-macro.lua game.gb
# macro=done frame=92
# system=gb frames=600 screen=0 width=160 height=144 crc32=…
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
| | | | | Full screen | ⌃⌘F |
| | | | | Switch player (link play) | ⌘T or click a screen |
| | | | | Save screenshot | ⌘⇧S |
| | | | | Start / stop recording | ⌘⇧R |
| | | | | Run / stop a Lua script | ⌘⇧L / ⌘⌥L |
| | | | | Colour correction on/off | ⌘⇧C |
| | | | | Filter: none, LCD grid, scanlines, Scale2x, Scale3x | View > Filter |
| | | | | Debugger | ⌘⇧D |
| | | | | Step / step over / next frame (debugger) | ⌘⇧I / ⌘⇧O / ⌘⇧N |
| | | | | 3D resolution (DS) | View > 3D Resolution |
| | | | | Original Game Boy games in colour or grey | View > Original Game Boy Games |
| | | | | Computer's microphone on/off (DS) | Emulation > Use Microphone |
| | | | | Fast-forward speed: 2×, 3×, 4× or unlimited | Emulation > Fast-Forward Speed |
| | | | | Show / hide frame rate and speed | ⌘⌥F |

The keys are laid out for playing on a keyboard: the left hand on W A S D
with the shoulders on Q and E, the right hand on a square of four keys
shaped like the DS face buttons, U and I (Y and X) above J and K (B and A).

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
gap, 3D resolution and filter are remembered in
`~/Library/Application Support/ds-emulator/settings.toml`.

View > Filter changes how the screens look. LCD Grid draws a thin dark
line between pixels, as on the handhelds' screens; Scanlines darkens the
bottom of each row, as on a CRT. Both need at least two of your display's
pixels to each console pixel, and show nothing in a smaller window. Scale2x
and Scale3x round off the staircase edges of sprites and text. They skip
DS frames drawn at a higher 3D resolution, whose 3D is smooth already.
Filters apply after colour correction and turn with the screens. xBRZ is
not offered: its code is GPL-3.0, which the project doesn't take (see the
[legal policy](docs/PRD.md#9-legal-policy)).

View > 3D Resolution draws DS 3D at 2×, 3× or 4× its own resolution
(`resolution_3d` under `[video]` in settings.toml goes up to 8): polygon
edges, textures and shading come out smooth, and the 2D layers around
them stay pixel for pixel. The game still sees native 3D wherever it reads
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
| `emu-cheats` | Cheat list parsing and application |
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

The Game Boy Color's palettes for original Game Boy games come from
SameBoy's open-source boot ROM, copyright © 2015–2026 Lior Halphon, under
the Expat (MIT) license.

Recordings are encoded with the `jpeg-encoder` crate, which is based in
part on the work of the Independent JPEG Group.

Scripts run on Lua 5.4, copyright © 1994–2026 Lua.org, PUC-Rio, built in
through the `mlua` crate; both are under the MIT license.
