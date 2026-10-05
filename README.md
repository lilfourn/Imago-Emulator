<img src="assets/icon.svg" width="64" height="64" alt="">

# ds-emulator

A Game Boy, Game Boy Color, Game Boy Advance and Nintendo DS emulator for
macOS, written in Rust with a [GPUI](https://gpui.rs) frontend. The goal is to
play the Pokémon games of generations 1–5 from your own cartridge dumps.

**Status: scaffold.** The frontend, core API, save states and test tooling
work end to end. The cores validate cartridge headers and draw a test pattern;
no CPU is emulated yet. Next up: the Game Boy's SM83 CPU
([roadmap](docs/ROADMAP.md), milestone M1).

## Docs

- [PRD](docs/PRD.md): goals, requirements, legal policy, risks, open questions
- [Roadmap](docs/ROADMAP.md): phases and milestones, GB → GBC → GBA → DS
- [Architecture](docs/ARCHITECTURE.md): crates, the core contract, threads, file formats
- [Decisions](docs/decisions/): why Rust and GPUI

## Build and run

Needs Rust (the pinned toolchain installs itself through `rustup`) and Xcode.
The separate Metal Toolchain download is not needed.

```sh
cargo run --release -p emu-app -- path/to/game.nds   # or open one with ⌘O
cargo test --workspace
```

Run a core without a window, for tests and scripting:

```sh
cargo run -p emu-headless -- --frames 600 --dump /tmp/frames path/to/game.gba
# system=gba frames=600 screen=0 width=240 height=160 crc32=…
```

## Controls

| Game | Key | | App | Shortcut |
|---|---|---|---|---|
| D-pad | Arrow keys | | Open ROM | ⌘O |
| A / B | X / Z | | Pause / resume | ⌘P |
| X / Y (DS) | S / A | | Reset | ⌘R |
| L / R | Q / W | | Save / load state | ⌘S / ⌘L |
| Start / Select | Return / Delete | | Stacked / side by side / one screen | ⌘1 / ⌘2 / ⌘3 |
| Touchscreen | Click and drag | | Swap screens | ⌘E |
| Fast-forward | Hold Tab | | Quit | ⌘Q |

Rebind game keys in `~/Library/Application Support/ds-emulator/bindings.toml`:

```toml
[keys]
a = ["x", "k"]     # several keys per action
fast_forward = ["tab"]
```

Battery saves (`game.sav`) and save states (`game.ss1`) are written next to
the ROM.

## Workspace

| Crate | Purpose |
|---|---|
| `emu-core` | The `Core` trait and the plain data that crosses it |
| `emu-sched` | Cycle-timestamped event scheduler |
| `emu-state` | Save-state container format |
| `emu-arm` | ARM interpreter shared by GBA and DS (not started) |
| `emu-gb`, `emu-gba`, `emu-nds` | One core per system (stubs today) |
| `emu-cores` | Picks the core for a ROM |
| `emu-headless` | Windowless runner for test ROMs |
| `emu-app` | The GPUI app (`ds-emulator` binary) |

## Legal

This project contains no Nintendo code, BIOS, firmware or games, and never
will. Dump your own cartridges and system files from hardware you own. Don't
open issues or pull requests that link to ROMs or BIOS files. Games, BIOS
dumps and saves are git-ignored, and CI fails if one is committed. See the
[legal policy](docs/PRD.md#9-legal-policy).
