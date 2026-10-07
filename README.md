<img src="assets/icon.svg" width="64" height="64" alt="">

# ds-emulator

A Game Boy, Game Boy Color, Game Boy Advance and Nintendo DS emulator for
macOS, written in Rust with a [GPUI](https://gpui.rs) frontend. The goal is to
play the Pokémon games of generations 1–5 from your own cartridge dumps.

**Status: Phase 4 (Nintendo DS).** The DS core direct-boots retail games
with an HLE BIOS. Pokémon White plays from power-on through a new game to
Route 1, and its in-game save survives a restart. It has:
- an ARM946E-S and an ARM7TDMI in lockstep, with IPC;
- both 2D engines and a software 3D engine with anti-aliasing;
- 16-channel sound with capture;
- the game card with flash saves;
- the GBA slot, which reads a GBA game and its save for Pal Park;
- the touchscreen, firmware and RTC.
It passes every test in RockPolish's rockwrestler and every page of
armwrestler for the DS.

The Game Boy and Game Boy Color core passes 135 of 158 open-source test
ROMs, including all of Blargg's CPU tests, dmg-acid2 and cgb-acid2. The Game
Boy Advance core has:
- an ARM7TDMI that passes all 2.25 million SingleStepTests cases;
- video, sound, DMA, timers and an HLE BIOS;
- SRAM, Flash and EEPROM saves;
- the Pokémon real-time clock;
- a link cable.
It passes every jsmolka test ROM it runs, including the save tests, and
every page of armwrestler. It passes 4,547 of the mGBA test suite's 6,998
checks: all of the shifter and carry tests, few of the timer ones. Neither
core has been checked against commercial games yet; that needs your own
dumps. The app has:
- sound and gamepads;
- save-state slots with thumbnails, rewind and fast-forward;
- link play;
- GBC/GBA colour correction;
- cheats and a memory viewer.

See the [roadmap](docs/ROADMAP.md).

## Docs

- [PRD](docs/PRD.md): goals, requirements, legal policy, risks, open questions
- [Roadmap](docs/ROADMAP.md): phases and milestones, GB → GBC → GBA → DS
- [Architecture](docs/ARCHITECTURE.md): crates, the core contract, threads, file formats
- [Decisions](docs/decisions/): why Rust and GPUI, Game Boy timing, frame pacing, linked cores, video filters, the GBA slot

## Build and run

Needs Rust (the pinned toolchain installs itself through `rustup`) and Xcode.
The separate Metal Toolchain download is not needed.

```sh
cargo run --release -p emu-app -- path/to/game.nds   # or open one with ⌘O
cargo run --release -p emu-app -- red.gb blue.gb     # two Game Boys, linked
cargo run --release -p emu-app -- fr.gba em.gba      # two GBAs, linked
cargo run --release -p emu-app -- pt.nds em.gba      # a GBA game in the DS's GBA slot
cargo test --workspace
```

Run a core without a window, for tests and scripting:

```sh
cargo run -p emu-headless -- --frames 600 --dump /tmp/frames path/to/game.gba
# system=gba frames=600 screen=0 width=240 height=160 crc32=…
```

## Controls

| Game | Key | Gamepad | | App | Shortcut |
|---|---|---|---|---|---|
| D-pad | W A S D or arrow keys | D-pad, left stick | | Open ROM | ⌘O |
| A / B | K / J | East / South | | Pause / resume | ⌘P |
| X / Y (DS) | I / U | North / West | | Reset | ⌘R |
| L / R | Q / E | Shoulder buttons | | Save / load state | ⌘S / ⌘L |
| Start / Select | Return / Delete | Start / Select | | Next / previous state slot | ⌘] / ⌘[ |
| Touchscreen | Click and drag | Right stick moves a cursor; click a stick to touch | | Fast-forward / slow motion on/off | ⌘F / ⌘⇧F |
| Fast-forward | Hold Tab | Right trigger | | Stacked / side by side / one screen / hybrid | ⌘1 / ⌘2 / ⌘3 / ⌘4 |
| Slow motion | Hold 1 | | | Swap screens / Quit | ⌘E / ⌘Q |
| Rewind | Hold R | Left trigger | | Rotate clockwise / anticlockwise | ⌘⌥→ / ⌘⌥← |
| | | | | Screen gap: 0, 16, 48 or 90 px | ⌘G |
| | | | | Full screen | ⌃⌘F |
| | | | | Switch player (link play) | ⌘T or click a screen |
| | | | | Colour correction on/off | ⌘⇧C |
| | | | | Memory viewer | ⌘⇧M |

The keys are laid out for playing on a keyboard: the left hand on W A S D
with the shoulders on Q and E, the right hand on a square of four keys
shaped like the DS face buttons, U and I (Y and X) above J and K (B and A).

The DS screens start stacked with no gap, in a window that opens as large
as fits the display; full screen shows them at the display's full height,
on black. Hybrid shows one screen large beside both screens small, as in
melonDS; Swap Screens chooses the large one. Rotation turns the whole
arrangement for games played with the DS held like a book, and touch
follows it. A 90 px gap matches the DS hinge. The layout, swap, rotation
and gap are remembered in
`~/Library/Application Support/ds-emulator/settings.toml`.

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

Battery saves (`game.sav`), save states (`game.ss1` to `game.ss4`) and
cheats (`game.cht`) live next to the ROM. A linked pair keeps each game's
`.sav` and `.cht`, and saves its states next to the first ROM as `game.ls1`
to `game.ls4`. GBA `.sav` files hold the raw save memory, as in mGBA.

## Link play

Open a Game Boy or Game Boy Advance game, choose File > Connect Link
Cable…, and pick a second game for the same system (or the same one with
another save). Both run side by side on one cable; on the GBA the first
game is the parent. Keyboard and gamepad control one console at a time:
click its screen or press ⌘T to switch. Game Boy `.sav` files use the same
layout as SameBoy, mGBA and BGB, including the MBC3 clock, so saves move
between them.

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
| `emu-cores` | Picks the core for a ROM |
| `emu-headless` | Windowless runner for test ROMs |
| `emu-app` | The GPUI app (`ds-emulator` binary) |

## Legal

This project contains no Nintendo code, BIOS, firmware or games, and never
will. Dump your own cartridges and system files from hardware you own. Don't
open issues or pull requests that link to ROMs or BIOS files. Games, BIOS
dumps and saves are git-ignored, and CI fails if one is committed. See the
[legal policy](docs/PRD.md#9-legal-policy).
