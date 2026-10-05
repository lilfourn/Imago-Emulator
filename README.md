<img src="assets/icon.svg" width="64" height="64" alt="">

# ds-emulator

A Game Boy, Game Boy Color, Game Boy Advance and Nintendo DS emulator for
macOS, written in Rust with a [GPUI](https://gpui.rs) frontend. The goal is to
play the Pokémon games of generations 1–5 from your own cartridge dumps.

**Status: Phase 2 (Game Boy Color).** The Game Boy and Game Boy Color core
runs: CPU, video, sound, MBC1/2/3/5/30 cartridges with battery saves and the
MBC3 clock, and a link cable between two Game Boys. It passes 135 of 158
open-source test ROMs, including all of Blargg's CPU tests, dmg-acid2 and
cgb-acid2. The app has sound, gamepads, save-state slots with thumbnails,
rewind and fast-forward. The GBA and DS cores are still stubs that draw a
test pattern. See the [roadmap](docs/ROADMAP.md).

## Docs

- [PRD](docs/PRD.md): goals, requirements, legal policy, risks, open questions
- [Roadmap](docs/ROADMAP.md): phases and milestones, GB → GBC → GBA → DS
- [Architecture](docs/ARCHITECTURE.md): crates, the core contract, threads, file formats
- [Decisions](docs/decisions/): why Rust and GPUI, Game Boy timing, frame pacing, linked cores

## Build and run

Needs Rust (the pinned toolchain installs itself through `rustup`) and Xcode.
The separate Metal Toolchain download is not needed.

```sh
cargo run --release -p emu-app -- path/to/game.nds   # or open one with ⌘O
cargo run --release -p emu-app -- red.gb blue.gb     # two Game Boys, linked
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
| D-pad | Arrow keys | D-pad, left stick | | Open ROM | ⌘O |
| A / B | X / Z | East / South | | Pause / resume | ⌘P |
| X / Y (DS) | S / A | North / West | | Reset | ⌘R |
| L / R | Q / W | Shoulder buttons | | Save / load state | ⌘S / ⌘L |
| Start / Select | Return / Delete | Start / Select | | Next / previous state slot | ⌘] / ⌘[ |
| Touchscreen | Click and drag | | | Fast-forward on/off | ⌘F |
| Fast-forward | Hold Tab | Right trigger | | Stacked / side by side / one screen | ⌘1 / ⌘2 / ⌘3 |
| Rewind | Hold ` | Left trigger | | Swap screens / Quit | ⌘E / ⌘Q |
| | | | | Switch player (link play) | ⌘T or click a screen |

Gamepad buttons are named by position, so A is the right face button as on a
Nintendo controller. Rebind keys and gamepad controls in
`~/Library/Application Support/ds-emulator/bindings.toml`:

```toml
[keys]
a = ["x", "k"]          # several keys per action
fast_forward = ["tab"]

[gamepad]
a = ["South"]           # Xbox-style: A on the bottom button
b = ["East"]

[gamepads.030000005e0400008e02000000007200]   # one controller, by UUID
rewind = ["LeftTrigger"]
```

Battery saves (`game.sav`) and save states (`game.ss1` to `game.ss4`) are
written next to the ROM. A linked pair keeps each game's `.sav` and saves
its states next to the first ROM as `game.ls1` to `game.ls4`.

## Link play

Open a Game Boy game, choose File > Connect Link Cable…, and pick a second
game (or the same one with another save). Both run side by side on one
cable. Keyboard and gamepad control one Game Boy at a time: click its
screen or press ⌘T to switch. Game Boy `.sav` files use the same layout as
SameBoy, mGBA and BGB, including the MBC3 clock, so saves move between them.

## Testing against test ROMs

```sh
scripts/fetch-test-roms.sh                                   # once
cargo test -p emu-gb --test test_roms -- --nocapture         # pass table
cargo run --release -p emu-gb --example trace -- rom.gb 1000 # CPU trace
```

`crates/emu-gb/tests/test-roms-passing.txt` lists every ROM known to pass;
the test fails if one regresses. Record new passes with `UPDATE_PASSING=1`.

## Workspace

| Crate | Purpose |
|---|---|
| `emu-core` | The `Core` trait and the plain data that crosses it |
| `emu-sched` | Cycle-timestamped event scheduler |
| `emu-state` | Save-state container format |
| `emu-arm` | ARM interpreter shared by GBA and DS (not started) |
| `emu-gb` | Game Boy core |
| `emu-gba`, `emu-nds` | GBA and DS cores (stubs today) |
| `emu-cores` | Picks the core for a ROM |
| `emu-headless` | Windowless runner for test ROMs |
| `emu-app` | The GPUI app (`ds-emulator` binary) |

## Legal

This project contains no Nintendo code, BIOS, firmware or games, and never
will. Dump your own cartridges and system files from hardware you own. Don't
open issues or pull requests that link to ROMs or BIOS files. Games, BIOS
dumps and saves are git-ignored, and CI fails if one is committed. See the
[legal policy](docs/PRD.md#9-legal-policy).
