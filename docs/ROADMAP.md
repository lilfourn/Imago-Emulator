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

**Status (2026-10-05):** M1–M3 met on test ROMs; M4 met except state-slot
thumbnails. 117 of 139 test ROMs pass
(`crates/emu-gb/tests/test-roms-passing.txt`). The Pokémon checkpoints need
your own cartridge dumps and have not been run.

| Not yet passing | Why | Matters for games? |
|---|---|---|
| Mooneye PPU `intr_2_*`, `lcdon_*`, `stat_lyc_onoff` (7) | STAT mode bits lag the mode-2 interrupt by part of an M-cycle; the scanline renderer does not model it | Rarely; a few raster-effect demos |
| Mooneye `boot_div`, `serial/boot_sclk_align` | Exact DIV and serial-clock phase left by the boot ROM | No |
| Mooneye `timer/rapid_toggle` | TAC toggling glitch | No |
| Mooneye MBC1 multicart | MBC1M wiring for compilation cartridges | Only those carts |
| Blargg `dmg_sound` 09, 10, 12 | Wave-RAM access races while channel 3 plays | Rarely |
| Blargg `oam_bug` | DMG OAM corruption bug | No (games avoid it) |

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

### M4. Save states, rewind and controllers ✅ (thumbnails pending)

- Complete `save_state`/`load_state` for the GB core.
- **Frontend:** rewind ring buffer (UI-11), state slots with thumbnails
  (UI-9), fast-forward toggle and speed cap (UI-10), gamepads through
  `gilrs` with per-controller profiles (UI-6).
- **Exit:** most Mooneye acceptance tests pass (registers B/C/D/E/H/L hold
  3/5/8/13/21/34). Rewind works. A gamepad profile survives restart.

---

## Phase 2 — Game Boy Color

### M5. CGB mode and the MBC3 clock

- Palettes, VRAM bank 1 attributes, WRAM banking, HDMA, double speed. MBC3
  RTC with the latch sequence and day-counter carry, saved as the 48-byte
  trailer (read 44- and 48-byte forms).
- **Exit:** cgb-acid2 passes. Pokémon Crystal's clock survives an emulator
  restart, day/night works, and Yellow runs with CGB palettes.

### M6. Link cable

- Serial transfer between two GB cores in one process.
- **Exit:** trade a Pokémon between two instances of Red/Blue or
  Gold/Silver.

---

## Phase 3 — Game Boy Advance

### M7. ARM7TDMI in `emu-arm`

- ARM and Thumb, barrel shifter, banked modes, exceptions, pipeline PC
  offsets. A `Bus` trait so the GBA and both DS CPUs can share the
  interpreter.
- **Frontend:** first debugger panels (UI-12): disassembly, registers,
  memory.
- **Exit:** jsmolka `arm.gba` and `thumb.gba` pass, plus armwrestler.

### M8. Memory, DMA, timers, BIOS

- Memory map with wait states, DMA, timers, IRQs, HLE BIOS for SWI calls,
  and the GBA's events moved onto `emu-sched`.
- **Exit:** jsmolka memory and BIOS tests pass, along with most of the mGBA
  suite's timer and DMA tests.

### M9. Video and sound

- PPU modes 0–5, affine backgrounds, windows, blending, mosaic. APU with
  DMA sound FIFOs.
- **Exit:** FireRed boots and plays with correct music.

### M10. Saves and clock

- Save-type detection by library string, Flash 128 KB with the Macronix ID,
  a game-code override table, and the S-3511 RTC on GPIO.
- **Exit:** Emerald saves, never shows "The internal battery has run dry",
  and berries grow over real time. Ruby/Sapphire tides work.

### M11. Link, colour, cheats

- SIO link between two instances. GBA colour-correction filter (UI-3).
  Cheat engine (UI-13). Memory viewers (UI-12). Settle open question Q3
  (shader path).
- **Exit:** trade between FireRed and Emerald over the emulated link cable.

---

## Phase 4 — Nintendo DS

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
  using the GBA core's cartridge code.
- **Exit:** HeartGold/SoulSilver and Black 2/White 2 play through with no
  anti-piracy symptoms (for example, Pokémon gaining no EXP). Pal Park reads
  a GBA ROM and save from the GBA slot.

### M16. Upscaling and wireless

- 3D upscaling, microphone or "blow" button, local wireless between two
  instances.
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
| State-slot thumbnails and picker, fast-forward speed cap | UI-9, UI-10 | Phase 2 |
| Debugger panels | UI-12 | M7 → M11 |
| Colour correction, cheats, shader path decision | UI-3, UI-13 | M11 |
| Hybrid layout, rotation, gamepad touch | UI-2, UI-7 | M13 |
| 3D upscaling | NDS-9 | M16 |
| Shader chain, recording, achievements, scripting | UI-4, UI-14 | Later |
