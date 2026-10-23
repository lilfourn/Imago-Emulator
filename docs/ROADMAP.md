# ds-emulator — Roadmap

Milestones and exit criteria for the [PRD](PRD.md), adapted from the
research report's step-by-step plan for this repository's Rust/GPUI stack.

**Pace assumption:** 10–15 hours a week. The report's estimates are judgment
calls, not measurements, so treat the dates as a budget to check against,
not a promise. Calendar targets assume a start in October 2026.

| Phase | Systems | Effort | Target |
|---|---|---|---|
| 0. Foundations | Scaffold ✅, optional CHIP-8 | 4–8 weeks | Nov 2026 |
| 1. Game Boy | GB (DMG) | ~3 months | Feb 2027 |
| 2. Game Boy Color | GBC | 1–2 months | Mar 2027 |
| 3. Game Boy Advance | GBA, `emu-arm` ARMv4T | 4–8 months | Oct 2027 |
| 4. Nintendo DS | DS, `emu-arm` ARMv5TE | 12–24+ months | 2028–2029 |
| 5. Gen 6–9 path | Host existing cores | Ongoing | — |

Every milestone ends with a check that can be run: a test ROM passing in the
headless runner, or a Pokémon checkpoint reached with your own dump.

---

## Phase 0 — Foundations

### S. Scaffold ✅ (2026-10-05)

- Cargo workspace with shared lints, `unsafe` denied, Rust 1.96 pinned.
- `emu-core`: the `Core` trait and plain-data types (frames, input, media,
  backup, state errors).
- `emu-sched`: cycle-timestamped event scheduler with deterministic ordering
  and save-state support. Tested.
- `emu-state`: save-state container (magic, system, state version, ROM
  CRC-32, payload, checksum). Tested.
- `emu-gb`, `emu-gba`, `emu-nds`: stub cores that validate cartridge headers
  and draw a test pattern. The pattern reacts to buttons and touch, so the
  whole frontend path can be exercised.
- `emu-cores`: picks a core by file extension.
- `emu-headless`: runs N frames, prints per-screen CRC-32s, dumps PPM.
- `emu-app` (GPUI): ROM loading by ⌘O, drag and drop, or command line;
  emulation thread with frame pacing; stacked, side-by-side and single
  layouts with swap; sharp-bilinear scaling; TOML key bindings; mouse
  touchscreen; fast-forward; save and load state; batched atomic battery
  saves.
- CI workflow and a no-Nintendo-files check.

### M0. CHIP-8 warm-up (optional)

A weekend interpreter in a scratch crate outside the workspace (CHIP-8's
16-key keypad does not fit the handheld button set, so it does not implement
`Core`).

- **Exit:** passes Timendus's CHIP-8 test suite; Pong is playable.

### Learning track (in parallel)

- *The Rust Programming Language* through chapter 10, plus chapter 15
  (smart pointers). Learn `u8`/`u16`/`u32` wrapping arithmetic, shifts, sign
  extension, and two's complement.
- How a CPU works: fetch–decode–execute, flags, stack, interrupts
  (*Code* by Charles Petzold, or Nand2Tetris parts 1–5).
- Read Pan Docs end to end before M1.

---

## Phase 1 — Game Boy

Frontend work in this phase: audio output, gamepads, the test-ROM suite and
the trace logger. Doing them now, while the core is small, is cheaper than
retrofitting.

**Status (2026-10-08):** M1–M4 met on test ROMs. 152 of 159 test ROMs pass
(`crates/emu-gb/tests/test-roms-passing.txt`), including every Mooneye
acceptance test the harness runs on a DMG and all of Blargg's `dmg_sound`.
The Pokémon checkpoints need your own cartridge dumps and have not been run.

| Not yet passing | Why | Matters for games? |
|---|---|---|
| Blargg `oam_bug` 2, 4, 5, 7, 8 and the combined ROM (6) | The DMG's OAM corruption bug is not emulated | No (games avoid it) |

Fixed on 2026-10-08: STAT now reports each mode a dot after the PPU enters
it and the PPU locks VRAM and OAM (Mooneye `intr_2_*`, `lcdon_*`,
`stat_lyc_onoff`, Blargg `oam_bug` 1); DIV and the serial clock keep the
boot ROM's phase (`boot_div`, `boot_sclk_align`); an overflow from stopping
the timer reloads at once (`rapid_toggle`); MBC1M multicarts; and DMG wave
RAM while channel 3 plays (`dmg_sound` 09, 10, 12).

### M1. SM83 CPU ✅

- All opcodes and CB-prefixed opcodes, flags, interrupts, HALT/STOP, EI
  delay. Serial port output captured for tests (writes to 0xFF01/0xFF02).
- **Frontend:** test-ROM manifest and runner script (TOOL-2); trace logger
  (TOOL-3); a script that downloads the c-sp/game-boy-test-roms bundle into
  the git-ignored `test-roms/`.
- **Exit:** Blargg `cpu_instrs` prints "Passed" over serial in the headless
  runner, then `instr_timing` passes.

### M2. Timer, PPU, joypad ✅ (test ROMs)

- Timer/DIV, interrupts, scanline PPU (background, window, sprites with
  priority and the 10-per-line limit), joypad. Replace the test pattern.
- **Exit:** dmg-acid2 frame CRC matches the reference. Tetris and Pokémon
  Red/Blue reach the title screen.

### M3. Cartridges, saves and sound ✅ (test ROMs)

- MBC1, MBC2, MBC3 (with clock) and MBC5, battery RAM through
  `Core::save_backup`, APU (all four channels).
- **Frontend:** `cpal` audio output with resampling and dynamic rate
  control (UI-8); settle open question Q4 (audio- vs vsync-driven pacing).
- **Exit:** Pokémon Red plays with sound; saving in game and restarting the
  app keeps the save. Most of Blargg `dmg_sound` passes.

### M4. Save states, rewind and controllers ✅

- Complete `save_state`/`load_state` for the GB core.
- **Frontend:** rewind ring buffer (UI-11), state slots with thumbnails
  (UI-9), fast-forward toggle and speed cap (UI-10), gamepads through
  `gilrs` with per-controller profiles (UI-6).
- **Exit:** most Mooneye acceptance tests pass (registers B/C/D/E/H/L hold
  3/5/8/13/21/34). Rewind works. A gamepad profile survives restart.

---

## Phase 2 — Game Boy Color

**Status (2026-10-08):** M5 and M6 met on test ROMs and unit tests. 152 of
159 test ROMs pass. Games made for the original Game Boy run on a Game Boy
Color in DMG mode by default, coloured as its boot ROM colours them: by
title for Nintendo's games, from SameBoy's copy of the palette table (PRD
Q5), or with one of the twelve button-combination palettes. dmg-acid2 run
that way matches the CGB reference screenshot; View > Original Game Boy
Games switches back to grey. Pokémon Red and Blue pick their red and blue
palettes from the table, but without the dumps neither has been seen in
game. Still open: `cgb-acid-hell` (2 pixels, mid-scanline effects). The
Pokémon checkpoints still need your own dumps.

### M5. CGB mode and the MBC3 clock ✅ (test ROMs)

- Palettes, VRAM bank 1 attributes, WRAM banking, HDMA, double speed. MBC3
  RTC with the latch sequence and day-counter carry, saved as the 48-byte
  trailer (read 44- and 48-byte forms).
- DMG mode on a Game Boy Color for games made for the original Game Boy,
  with the boot ROM's compatibility palettes.
- **Exit:** cgb-acid2 passes. Pokémon Crystal's clock survives an emulator
  restart, day/night works, and Yellow runs with CGB palettes.

### M6. Link cable ✅ (unit tests)

- Serial transfer between two GB cores in one process: `emu_gb::Linked`
  runs both in 8-M-cycle lockstep slices ([decision 0004](decisions/0004-linked-cores.md)).
  Open a game, then File > Connect Second Console…, or pass two ROMs on
  the command line. Click a screen or press ⌘T to control the other Game Boy.
- **Exit:** trade a Pokémon between two instances of Red/Blue or
  Gold/Silver.

---

## Phase 3 — Game Boy Advance

**Status (2026-10-07):** M7–M11 met on test ROMs and unit tests:
- the ARM7TDMI passes all 45 SingleStepTests files (2.25 million cases),
  multiply carry flag included;
- jsmolka's arm, thumb, memory, bios, nes and save tests all pass;
- armwrestler passes all eight of its pages;
- the mGBA test suite passes 6,968 of the 6,998 checks in its 13
  self-checking groups (`crates/emu-gba/tests/mgba-suite-passing.txt`).
What's missing:
- 30 mGBA suite checks (see M8): CpuFastSet's timing, serial transfer
  timing, a lone console's multi-player SIOCNT, and HBlank timings
  measured around HALT.
The Pokémon checkpoints (FireRed music, Emerald saves and clock, the
FireRed–Emerald trade) need your own dumps.

### M7. ARM7TDMI in `emu-arm` ✅ (test ROMs)

- ARM and Thumb, barrel shifter, banked modes, exceptions, pipeline PC
  offsets. A `Bus` trait so the GBA and both DS CPUs can share the
  interpreter.
- **Frontend:** first debugger panels (UI-12): disassembly, registers,
  memory. ✅ View > Debugger, for every system: ARM/Thumb (ARMv4T and
  ARMv5TE) and SM83 disassemblers, breakpoints and watchpoints that stop
  mid-frame, step, step over and frame advance, registers, a memory
  editor, tile/map/sprite/palette viewers, I/O registers and the
  scheduler's queue.
- **Exit:** jsmolka `arm.gba` and `thumb.gba` pass, plus armwrestler.

### M8. Memory, DMA, timers, BIOS ✅ (jsmolka, mGBA suite)

- Memory map with wait states and the Game Pak prefetch buffer, DMA,
  timers, IRQs, HLE BIOS for SWI calls, and the GBA's events moved onto
  `emu-sched`.
- **Exit:** jsmolka memory and BIOS tests pass, along with most of the mGBA
  suite's timer and DMA tests.
- mGBA suite, per group: memory 1,552/1,552, I/O read 130/130, timing
  2,000/2,020, timer count-up 936/936, timer IRQ 90/90, shifter 140/140,
  carry 93/93, multiply long 72/72, BIOS math 615/615, DMA 1,244/1,244, SIO
  registers 89/90, SIO timing 0/4, misc. edge cases 7/12.
- Getting there fixed, among others: interrupts, timer writes and timer
  starts land with the hardware's few-cycle delays, and the prescalers
  share one clock; DMA starts two cycles after its trigger, takes the bus
  at the CPU's next access and keeps a per-channel latch; ROM opcodes
  come through the prefetch buffer, which most games turn on; HLE BIOS
  calls take the real BIOS's time; ArcTan2 takes 32-bit coordinates;
  write-only registers read as open bus. Earlier, the suite found that
  VBlankIntrWait never returned under the HLE BIOS.
- Left: CpuFastSet's loop overhead (20 timing checks), serial transfer
  timing (4), a lone console's multi-player SIOCNT (1), and five misc.
  checks: HBlank flag timings measured around HALT and open bus while an
  HBlank DMA repeats.

### M9. Video and sound ✅ (implemented; needs a dump to check)

- PPU modes 0–5, affine backgrounds, windows, blending, mosaic. APU with
  DMA sound FIFOs.
- **Exit:** FireRed boots and plays with correct music.

### M10. Saves and clock ✅ (test ROMs)

- Save-type detection by library string, Flash 128 KB with the Macronix ID,
  a game-code override table, and the S-3511 RTC on GPIO.
- **Exit:** Emerald saves, never shows "The internal battery has run dry",
  and berries grow over real time. Ruby/Sapphire tides work.

### M11. Link, colour, cheats ✅ (unit tests)

- SIO link between two instances. GBA colour-correction filter (UI-3).
  Cheat engine (UI-13). Memory viewers (UI-12). Settle open question Q3
  (shader path).
- Done:
  - `emu_gba::Linked` (normal and multi-player modes);
  - colour correction for GBC and GBA (View > Colour Correction);
  - `emu-cheats` with `game.cht` files (File > Edit Cheats…): device
    tags or detection; GameShark Advance / Action Replay v1/v2 and Pro
    Action Replay v3, encrypted (both seed sets, `DEADFACE`) or not;
    CodeBreaker with its encryption; Game Boy GameShark bank codes and
    Game Genie; ROM patches (Game Genie, GameShark Advance `6`, Pro Action
    Replay `18`–`1E`) and Pro Action Replay I/O writes (`C6`, `C7`); and
    Action Replay DS for Phase 4. Cheats run after each frame, at VBlank;
  - a memory viewer, since grown into the debugger (View > Debugger);
  - [decision 0005](decisions/0005-video-filters-on-the-cpu.md) for Q3,
    accepted once the filters were built on it in Phase 4.
- **Exit:** trade between FireRed and Emerald over the emulated link cable.

---

## Phase 4 — Nintendo DS

**Status (2026-10-07):** M12–M14 are implemented and M13's frontend items
are done.
- The DS core direct-boots retail games with an HLE BIOS, including the
  callback decompressors and SoftReset.
- Pokémon White plays from power-on through a new game to the 3D bedroom in
  Nuvema Town, then on to Route 1. Battles give EXP, so there are no
  anti-piracy symptoms. An in-game save survives a restart, and Continue
  loads it. The run is scripted through `examples/probe.rs` with your own
  dump, at about 3× full speed headless.
- 3D has edge anti-aliasing, edge marking, fog, toon shading and shadows.
  Sound has all 16 channels and both capture units.
- M16 so far: 3D draws at up to 8× its resolution (View > 3D Resolution),
  with the 2D layers enlarged around it; in White's bedroom, headless on an
  M5 Max, that runs at about 3.2× full speed at 2× and 2.7× at 4×. The
  microphone hears a blow key (B) or the computer's microphone.
- Local wireless: two consoles in one process share a clock, and the ARM7's
  Wi-Fi chip sends beacons, connects and runs Nintendo's multiplayer
  exchange ([decision 0007](decisions/0007-ds-local-wireless.md)). Two
  Pokémon White games, each from a save at the C-Gear, see each other in
  the Union Room, talk, and trade (Tepig for Patrat, scripted through
  `examples/link_probe.rs`). The pair runs at about 1.6× full speed
  headless; one console's speed is unchanged.
- RockPolish's rockwrestler passes all 23 of its tests, and armwrestler
  for the DS all eight of its pages, both checked by `cargo test`.
- VBlankIntrWait, which libnds calls as swiWaitForVBlank, used to wait
  forever under the HLE BIOS; the mGBA suite found the same bug on the GBA.
  Both are fixed.
- Action Replay DS cheats (`ards` in `game.cht`) run after each frame on
  main RAM, and button codes read KEYINPUT and EXTKEYIN. A button-activated
  pointer code changes only its target bytes in a White run (probe
  `CHEATS=`).

| M | Done | Left |
|---|---|---|
| M12 | ARMv5TE, CP15 with TCMs and MPU enforcement, lockstep CPUs, IPC; armwrestler-DS and rockwrestler pass | A libnds hello world: devkitPro publishes no built one, so it needs devkitARM to build |
| M13 | Direct boot, VRAM, both 2D engines, SPI devices, RTC; hybrid layout, rotation, gap, gamepad touch | Frame CRCs against melonDS (the test-ROM runner reads text off the screen, not CRCs) |
| M14 | 3D with anti-aliasing, sound with capture, 512 KB flash saves | The Platinum checkpoint (needs that dump) |
| M15 | Pokémon White shows no anti-piracy symptoms; GBA slot with SRAM and Flash saves ([decision 0006](decisions/0006-gba-slot.md)) | Timing against melonDS traces; HG/SS and B2/W2; Pal Park with real GBA dumps |
| M16 | 3D upscaling to 8× (NDS-9); microphone from a blow key or the computer's (NDS-10); local wireless, with a Union Room trade between two Pokémon Whites (NDS-10) | The exit's trade in Platinum or HG/SS (needs those dumps); DS Download Play |

### M12. Two CPUs

- ARMv5TE instructions and CP15 in `emu-arm`. ARM9/ARM7 interleaved
  scheduling, IPC sync and FIFO.
- **Exit:** armwrestler-DS passes and a libnds "hello world" built with
  devkitPro runs.

### M13. 2D, touch, boot

- Direct boot, VRAM banks A–I, both 2D engines, SPI touchscreen, firmware
  and power management, RTC, cartridge reads.
- **Frontend:** hybrid layout, rotation, gap setting (UI-2); "touch at
  cursor" for gamepads (UI-7).
- **Exit:** 2D homebrew frame CRCs match melonDS; simple commercial 2D games
  boot; touch works through every layout.

### M14. 3D and sound

- Geometry engine and software rasterizer; 16-channel sound with capture;
  512 KB flash saves.
- **Exit:** Pokémon Platinum reaches the overworld with correct 3D, and its
  save survives restart.

### M15. Accuracy

- Timing and cartridge behaviour, measured against melonDS traces. GBA slot
  using the GBA cartridge code, shared through `emu-gba-cart`.
- **Exit:** HeartGold/SoulSilver and Black 2/White 2 play through with no
  anti-piracy symptoms (for example, Pokémon gaining no EXP). Pal Park reads
  a GBA ROM and save from the GBA slot.

### M16. Upscaling and wireless

- 3D upscaling, microphone or "blow" button, local wireless between two
  instances.
- Done: 3D upscaling (`Core::set_render_scale`, View > 3D Resolution); the
  microphone as a loudness in `Input` (hold B, or Emulation > Use
  Microphone); local wireless between two consoles, `emu_nds::Linked`
  ([decision 0007](decisions/0007-ds-local-wireless.md)). Open a DS game,
  then File > Connect Second Console…, or pass two ROMs on the command
  line. Two Pokémon Whites trade in the Union Room; Platinum and HG/SS are
  untried.
- **Exit:** trade between two instances of Platinum or HeartGold/SoulSilver
  over emulated local wireless.

---

## Phase 5 — Gen 6–9, the realistic path

- A libretro-host mode so the frontend can load existing cores (for example,
  an Azahar-derived 3DS core) and still provide its own layouts, bindings and
  save-state UI. Or contribute to Azahar directly.
- For learning, write parts of a 3DS emulator (an ARM11 interpreter, a
  PICA200 shader interpreter) against homebrew. Do not plan to run X/Y or
  Sun/Moon on them.
- No Switch emulation work.

---

## Frontend track at a glance

| Feature | PRD | Lands in |
|---|---|---|
| Open, drag and drop, CLI | UI-1 | S ✅ |
| Stacked / side-by-side / single + swap | UI-2 | S ✅ |
| Sharp-bilinear scaling | UI-3 | S ✅ |
| Keyboard bindings (TOML) | UI-5 | S ✅ |
| Mouse touchscreen | UI-7 | S ✅ |
| Save/load state, one slot | UI-9 | S ✅ |
| Fast-forward (hold) | UI-10 | S ✅ |
| Battery saves, atomic and batched | CORE-4 | S ✅ |
| Test-ROM suite, trace logger | TOOL-2, TOOL-3 | M1 ✅ |
| Audio with dynamic rate control | UI-8 | M3 ✅ |
| Rewind, four state slots, fast-forward toggle, gamepads | UI-6, UI-9–11 | M4 ✅ |
| State-slot thumbnails in the slot notice | UI-9 | Phase 2 ✅ |
| Link cable, per-player input | GB-6 | M6 ✅ |
| Fast-forward speed cap, slow motion | UI-10 | Phase 4 ✅ |
| Per-game bindings, turbo, gamepad hotkey chords | UI-6 | Phase 4 ✅ |
| Debugger panels | UI-12 | M7 → M11 ✅ |
| Colour correction, cheats, shader path decision | UI-3, UI-13 | M11 |
| Hybrid layout, rotation, gap, gamepad touch | UI-2, UI-7 | M13 ✅ |
| Keyboard-first defaults, large window, full screen | UI-2, UI-5 | Phase 4 ✅ |
| 3D upscaling (View > 3D Resolution) | NDS-9 | M16 ✅ |
| Blow key, computer microphone | NDS-10 | M16 ✅ |
| Two DSes in wireless range, side by side | NDS-10 | M16 ✅ |
| Filters (LCD grid, scanlines, Scale2x/3x), screenshots, video recording | UI-4, UI-14 | Phase 4 ✅ |
| Lua scripting: File > Run Script…, `--script`, headless | UI-14 | Phase 4 ✅ |
| ROM-hack patches (IPS, UPS, BPS, xdelta); `.7z` and `.rar` games | UI-15 | Frontend ✅ |
| xBRZ, slang presets, achievements | UI-4, UI-14 | Later |
