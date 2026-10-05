# 0004 — Link play: a linked pair is one core with two players

- Status: accepted
- Date: 2026-10-05
- Deciders: Luke Fournier

## Context

Milestone M6 is trading a Pokémon between two Game Boys over an emulated
link cable; M11 and M16 need the same for GBA and DS. A serial transfer
takes 1,024 M-cycles and Pokémon's link protocol checks every byte, so the
two machines must stay within a small fraction of that of each other.
Running each in its own emulation thread, synchronised once per frame,
puts them up to 16 ms (about 17,000 M-cycles) apart.

## Decision

1. A linked pair is a system-specific core (`emu_gb::Linked`) that owns
   both machines and runs them in alternating slices of 32 M-cycles,
   exchanging serial bytes between slices. Each system knows its own cable;
   the frontend does not.
2. The core interface grows a notion of players:
   - `CoreInfo::players`, with screen `i` belonging to player
     `i * players / screens.len()`;
   - `Core::run_frame` takes one `Input` per player;
   - `Core::backup_generation` and `Core::save_backup` take a player index,
     because each player has their own cartridge and `.sav` file.
3. The app shows a linked pair like any multi-screen core. Keyboard and
   gamepad drive one player at a time; clicking a screen or ⌘T switches.

## Consequences

- Lockstep in one thread keeps link play deterministic, so rewind, save
  states and the test harness work for linked sessions unchanged.
- Every core and frontend changed signature once. Single-player cores read
  `inputs[0]` and ignore the player index for backups.
- Two-player local play on one keyboard means switching control. Mapping a
  second gamepad to player 2 is a later addition to the bindings.

## Addendum (M11): the GBA pair

`emu_gba::Linked` follows the same design with 128-cycle slices. Two
things differ from the Game Boy pair:
- It services the cable the moment either console starts a transfer,
  instead of waiting for the slice to end. A normal-mode transfer at 2 MHz
  lasts only 64 cycles.
- Player 0 is the multi-player parent.
