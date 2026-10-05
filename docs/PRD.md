# ds-emulator — Product Requirements

| | |
|---|---|
| Status | Draft 0.1 |
| Date | 2026-10-05 |
| Owner | Luke Fournier |
| Source | *Building Your Own Pokémon-Capable Emulator on a Mac: A Practical Roadmap* (October 2026), referred to below as "the research report" |
| Related | [ROADMAP.md](ROADMAP.md) · [ARCHITECTURE.md](ARCHITECTURE.md) · [decisions/](decisions/) |

## 1. Summary

ds-emulator is a macOS emulator for the Game Boy, Game Boy Color, Game Boy
Advance and Nintendo DS. It is written in Rust, with a native GPUI frontend.
It exists to play the Pokémon games of generations 1 to 5 from the user's own
cartridge dumps, and to teach its author how these machines work.

The research report sets the plan: one shared frontend, separate cores behind
a small libretro-style API, built in order GB → GBC → GBA → DS. Each system is
a stepping stone to the next. The DS is the destination.

## 2. Background

The research report concludes:

- GB, GBC, GBA and DS are "bare metal" systems. Games talk to hardware
  registers that Pan Docs and GBATEK document completely. One person can
  emulate them: GB/GBC in roughly 2–4 months part-time, GBA in 4–8 more,
  and the DS in 1–2 years or more.
- 3DS and Switch require high-level emulation of a whole operating system, a
  GPU-to-Metal shader translator and in practice a JIT. Citra, yuzu and
  Ryujinx were multi-year team efforts. Switch work also carries the highest
  legal risk. Both are out of scope.
- The frontend should be separate from the cores. Then remapping, save
  states, rewind, fast-forward and shaders are written once, every core can
  run headless for automated tests, and third-party cores could be hosted
  later.
- Pokémon games stress specific hardware: real-time clocks (MBC3, Seiko
  S-3511), exact save types (GBA 128 KB flash, DS 512 KB flash), and the
  HG/SS and B/W anti-piracy checks that punish inaccurate emulation.

Two decisions differ from the report's primary recommendation. Both are
recorded in [decisions/0001-rust-and-gpui.md](decisions/0001-rust-and-gpui.md):

- **Rust and Cargo instead of C++20 and CMake.** The report calls Rust an
  excellent alternative.
- **GPUI instead of SDL3 and Dear ImGui.** GPUI owns the window and main run
  loop, so audio uses `cpal` and gamepads use `gilrs` instead of SDL3.

## 3. Goals

| ID | Goal |
|---|---|
| G1 | Play every mainline Pokémon game of generations 1 to 5 from start to credits from the user's own dumps, with working battery saves and real-time clock. |
| G2 | One frontend for every system. A feature written once (save states, rewind, remapping, layouts, shaders) works for every core. |
| G3 | Prove accuracy with automated test ROMs on every commit, not just by playing. |
| G4 | Feel native on Apple Silicon Macs: Retina-sharp, smooth on ProMotion displays, standard menus and shortcuts. |
| G5 | Stay readable as a learning project. Cores are interpreters written to be understood, with documentation references next to the code. |
| G6 | Stay legally clean. No Nintendo code, BIOS, firmware, keys or games, in the repository or in releases. |

## 4. Non-goals

- 3DS and Switch cores. Gen 6–9 support, if it ever comes, means hosting an
  existing core (Phase 5 of the roadmap), never writing one.
- A JIT or dynamic recompiler. An interpreter runs GB, GBA and DS at full
  speed on M-series chips. Revisit only if the DS core measurably cannot.
- DSi mode, the GBA e-Reader, the GBA Wireless Adapter and the Pokéwalker in
  version 1.
- Windows and Linux releases in version 1. GPUI supports them, but they are
  not built or tested.
- Internet netplay in version 1. Local two-instance link play is in scope.
- Any decryption tooling or key handling.

## 5. Users

- **Primary: the developer.** Plays their own dumps on an M-series Mac and
  uses the debugger and trace logs while building the cores.
- **Secondary: Mac players who own their cartridges** and want a single,
  native app for GB through DS.

## 6. Requirements

Priority: **P0** is required for the milestone named, **P1** is wanted for
version 1, **P2** comes later. Milestones (M0–M16) are defined in
[ROADMAP.md](ROADMAP.md). ✅ marks requirements the scaffold already meets.

### 6.1 Core contract

| ID | Requirement | Priority |
|---|---|---|
| CORE-1 | Every system implements `emu_core::Core`. Frontends only use `dyn Core`. ✅ | P0 · S |
| CORE-2 | Cores are deterministic: the same media and input sequence produce the same frames, audio and state bytes. Cores never read the host clock, randomness or files. | P0 · S |
| CORE-3 | `save_state` captures the complete machine (CPU, memory, scheduler queue, peripherals). States are versioned per core and wrapped in a container with system tag, ROM CRC-32 and checksum. ✅ container | P0 · M4 |
| CORE-4 | Battery saves are exposed with a write counter. The frontend flushes them at most 1 s after a write, on quit, and atomically (write a temporary file, then rename). ✅ | P0 · M3 |
| CORE-5 | The real-time clock starts at `Media::start_time` and advances with emulated cycles. Per-game RTC offsets are supported. | P0 · M5, M10 |
| CORE-6 | No BIOS or firmware is required. Use an HLE GBA BIOS and DS direct boot by default; use the user's own dumps when provided. | P0 · M8, M13 |
| CORE-7 | Cores expose memory regions for debuggers, cheats and game tools. ✅ interface | P1 · M11 |

### 6.2 Systems

**Game Boy / Game Boy Color (`emu-gb`)**

| ID | Requirement | Priority |
|---|---|---|
| GB-1 | SM83 CPU: all opcodes including CB-prefixed, flags, interrupts, HALT/STOP, the EI delay, the HALT bug. | P0 · M1 |
| GB-2 | Timer/DIV, joypad, and a scanline PPU (background, window, sprites with priority and the 10-per-line limit). | P0 · M2 |
| GB-3 | APU: two square channels (one with sweep), wave and noise, with length, envelope and frame sequencer. | P0 · M3 |
| GB-4 | Cartridges: MBC1, MBC3 with RTC, MBC5, battery RAM. MBC3 RTC is saved in the de facto 48-byte trailer; 44- and 48-byte trailers are both read. | P0 · M3, M5 |
| GB-5 | CGB mode: palettes, VRAM bank 1 attributes, WRAM banking, HDMA, double speed. | P0 · M5 |
| GB-6 | Serial link cable between two instances in one process. | P1 · M6 |

**Game Boy Advance (`emu-gba`, `emu-arm`)**

| ID | Requirement | Priority |
|---|---|---|
| GBA-1 | ARM7TDMI interpreter: ARM and Thumb, barrel shifter, banked modes, exceptions, pipeline PC offsets. Lives in `emu-arm` for reuse by the DS. | P0 · M7 |
| GBA-2 | Memory map with per-region wait states, DMA, timers, IRQs, HLE BIOS, event scheduler. | P0 · M8 |
| GBA-3 | PPU modes 0–5, affine backgrounds, windows, blending, mosaic. APU with DMA sound FIFOs. | P0 · M9 |
| GBA-4 | Backup: SRAM, EEPROM (512 B and 8 KB), Flash 64 KB and 128 KB with correct chip IDs (e.g. Macronix 0x09C2). Detect the type from `FLASH1M_V`/`FLASH512_V`/`SRAM_V`/`EEPROM_V` strings, with a game-code override table. | P0 · M10 |
| GBA-5 | Seiko S-3511 RTC on GPIO, enabled automatically for Ruby, Sapphire and Emerald by game code. | P0 · M10 |
| GBA-6 | SIO link cable between two instances. | P1 · M11 |

**Nintendo DS (`emu-nds`, `emu-arm`)**

| ID | Requirement | Priority |
|---|---|---|
| NDS-1 | ARMv5TE additions (CLZ, QADD/QSUB, DSP multiplies, BLX, LDRD/STRD) and CP15 (protection unit, TCM, cache control). | P0 · M12 |
| NDS-2 | ARM9 and ARM7 run in interleaved slices, with IPC sync and FIFO. | P0 · M12 |
| NDS-3 | Direct boot, VRAM bank mapping (banks A–I), both 2D engines, SPI touchscreen/firmware/power management, RTC, cartridge reads. | P0 · M13 |
| NDS-4 | 3D: geometry engine (GXFIFO, matrix stacks, lighting, clipping) and a software rasterizer as the accuracy reference. | P0 · M14 |
| NDS-5 | Sound: 16 channels (PCM8, PCM16, IMA-ADPCM, PSG) and sound capture. | P0 · M14 |
| NDS-6 | 512 KB SPI flash saves for every DS Pokémon game. | P0 · M14 |
| NDS-7 | No anti-piracy symptoms in HG/SS, B/W or B2/W2 without patches. Any trigger is an emulator bug. | P0 · M15 |
| NDS-8 | GBA slot: insert a GBA ROM and save so Pal Park works. | P1 · M15 |
| NDS-9 | 3D upscaling (N× internal resolution, or a Metal compute rasterizer). | P1 · M16 |
| NDS-10 | Local wireless between two instances (trades, battles). Microphone input or a "blow" button. | P2 · M16 |
| NDS-11 | HG/SS infrared cartridge (Pokéwalker). | P2 |

### 6.3 Frontend (`emu-app`)

| ID | Requirement | Priority |
|---|---|---|
| UI-1 | Open a ROM from the File menu (⌘O), by drag and drop, or from the command line. ✅ | P0 · S |
| UI-2 | DS screen layouts: stacked, side by side, single screen with swap. ✅ Hybrid (one large and one small screen), rotation and gap setting. | P0 · S / P1 · M13 |
| UI-3 | Crisp scaling at any window size ("sharp bilinear"). ✅ Integer scaling, and GBA/GBC colour correction. | P0 · S / P1 · M11 |
| UI-4 | Shader and filter chain (LCD grid, CRT, xBRZ), ideally reading slang presets. | P2 |
| UI-5 | Input action layer: physical input → action → core buttons. Keyboard bindings in TOML with several keys per action. ✅ | P0 · S |
| UI-6 | Gamepads with per-controller profiles keyed by GUID, per-game overrides, turbo, analog-to-D-pad, hotkey chords. | P1 · M4 |
| UI-7 | Touch: mouse or trackpad drives the DS touchscreen through the inverse layout transform. ✅ A "touch at cursor" binding for gamepads. | P0 · S / P1 · M13 |
| UI-8 | Audio output at the device rate with dynamic rate control (adjust the resample ratio by up to ±0.5% based on buffer fill). | P0 · M3 |
| UI-9 | Save state slots with thumbnails. ✅ one slot (⌘S / ⌘L). | P0 · S / P1 · M4 |
| UI-10 | Fast-forward while held (Tab). ✅ Toggle, speed cap, slow motion. | P0 · S / P1 · M4 |
| UI-11 | Rewind: a ring buffer of states every N frames, XOR-delta compressed with zstd or LZ4. | P1 · M4 |
| UI-12 | Debugger panels: disassembly with breakpoints and watchpoints, registers, memory editor, tile/map/OAM/palette viewers, I/O registers, scheduler queue, trace logging. | P1 · M7 |
| UI-13 | Cheats: GameShark, Action Replay, CodeBreaker and raw patches. | P2 · M11 |
| UI-14 | Screenshots and video recording, RetroAchievements (rcheevos), Lua scripting hooks. | P2 |

### 6.4 Tooling

| ID | Requirement | Priority |
|---|---|---|
| TOOL-1 | Headless runner: runs N frames and prints one `key=value` line per screen with a CRC-32, and optionally dumps PPM images. ✅ | P0 · S |
| TOOL-2 | Test-ROM suite: a manifest of test ROMs and their pass criteria (serial output, Mooneye's Fibonacci registers, framebuffer CRC), a script that runs them all and prints a pass table, run in CI. Test ROMs are downloaded by script, never committed. | P0 · M1 |
| TOOL-3 | Trace logger whose format can be diffed against mGBA, SameBoy and melonDS logs. | P1 · M1 |
| TOOL-4 | CI on macOS: rustfmt, clippy with warnings denied, tests, and a check that no Nintendo files are tracked. ✅ written, not yet run on a remote | P0 · S |

## 7. Non-functional requirements

| Area | Requirement |
|---|---|
| Performance | Release builds on an M1: GB/GBC at 10× real time or more, GBA at 4× or more, DS at full speed (59.83 fps). The UI thread spends under 4 ms per frame. |
| Latency | Input reaches the core within 2 frames. |
| Determinism | Running the same ROM and input twice gives identical frame CRCs. Checked in CI. |
| Save safety | No save is ever lost to a crash or quit: atomic writes, flush on quit, flush within 1 s of a game write. |
| Save compatibility | `.sav` files interchange with mGBA, SameBoy and melonDS wherever a de facto format exists. |
| Platform | macOS on Apple Silicon is supported. Intel Macs should build but are not tested. |
| Code safety | `unsafe` is denied workspace-wide. Any exception needs a decision record and a `// SAFETY:` comment. |
| Build | `cargo build` works on a stock Xcode install without the separate Metal Toolchain download (GPUI's `runtime_shaders` feature). |

## 8. Success metrics

| Milestone | Automated bar | Pokémon checkpoint |
|---|---|---|
| M1 | Blargg `cpu_instrs` passes via serial output | — |
| M2 | dmg-acid2 frame CRC matches | Red/Blue reach the title screen |
| M3 | Most of Blargg `dmg_sound` | Red plays with sound; in-game save survives restart |
| M4 | Most Mooneye acceptance tests | — |
| M5 | cgb-acid2 | Crystal's clock survives an emulator restart; day/night works |
| M6 | — | Trade between two instances of Red/Blue or Gold/Silver |
| M7 | jsmolka `arm.gba`/`thumb.gba`, armwrestler | — |
| M8 | jsmolka memory/BIOS tests, most mGBA timer/DMA tests | — |
| M9 | — | FireRed plays with correct music |
| M10 | GBA RTC test ROM | Emerald saves, no "internal battery has run dry", berries grow; Ruby/Sapphire tides work |
| M11 | — | FireRed ↔ Emerald trade over emulated link cable |
| M12 | armwrestler-DS; libnds hello world | — |
| M13 | 2D homebrew frame CRCs match melonDS | Simple commercial 2D games boot; touch works |
| M14 | — | Platinum reaches the overworld with correct 3D and saves |
| M15 | — | HG/SS and B2/W2 play through with no anti-piracy symptoms; Pal Park reads a GBA slot cartridge |
| M16 | — | Trade between two instances of Platinum or HG/SS over local wireless |

## 9. Legal policy

Writing a clean-room emulator is generally lawful in the US (*Sega v.
Accolade*, *Sony v. Connectix*). The rules below follow the research report;
they are project policy, not legal advice.

1. Never commit or distribute games, BIOS, firmware, save files or keys.
   `.gitignore` excludes them and `scripts/check-no-nintendo-files.sh` fails
   CI if any are tracked.
2. Never link to sources of ROMs or BIOS files, in docs or in the app.
3. Default to HLE BIOS and direct boot. The app uses user-dumped system files
   only when the user points it at them.
4. Read GPL code to understand it, but do not copy it unless the project
   adopts a compatible license (see open question Q1). MIT, BSD and
   MPL-2.0 code may be reused under its terms, with attribution.
5. No decryption tooling, no 3DS or Switch keys, no builds tied to
   unreleased games, no paywalled builds.
6. Tests and docs never embed copyrighted game assets. Test ROMs used in CI
   are freely redistributable homebrew, downloaded at test time.

## 10. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| GPUI is young and pre-1.0. The crates.io release (0.2.2) trails Zed's main branch, and the API changes often. | Upgrades break the frontend. | Keep GPUI code inside `emu-app`. Pin the version. Upgrade on purpose, not in passing. |
| GPUI samples images linearly and has no custom-shader hook. Its Metal atlas does not reclaim image tiles when other images share a texture. | Blurry pixels; possible memory growth; no shader chain. | CPU integer pre-scale (sharp bilinear), already done. Memory measured flat with the current one-image-per-screen design. Shader chain needs a separate Metal or wgpu path (decision record 0001, open question Q3). |
| DS scope (two CPUs, 3D, VRAM banking, cartridge protocol) | The DS phase stalls or takes years. | GBA first, sharing `emu-arm`. Software 3D first. Diff traces against melonDS. Treat DS as a stretch goal with its own milestones. |
| Borrow checker vs. "everything points at the bus" designs | Slow progress on the first cores. | Each core owns its machine as one struct. The CPU borrows the bus per call (`cpu.step(&mut bus)`); no `Rc<RefCell<…>>` webs. |
| Retrofitting determinism and save states | Rewind and netplay become impossible later. | The contract requires both from the first core. The scaffold's stub cores already save and load state. |
| Motivation over a 2–3 year horizon | Project abandoned. | Every milestone ends with something playable or a test suite turning green. |
| Legal | Takedown. | Section 9. Out of scope: 3DS, Switch, keys. |

## 11. Open questions

| ID | Question | Needed by |
|---|---|---|
| Q1 | Which license? GPL-3.0-or-later would allow porting code from melonDS; MPL-2.0 matches mGBA; MIT or Apache-2.0 keeps reuse open. Choose before accepting outside contributions. | Before first public push |
| Q2 | Stay on crates.io GPUI 0.2.2, or pin a newer Zed git revision? | M3 |
| Q3 | Shader chain and DS upscaling: render through a separate Metal/wgpu layer, or contribute nearest-neighbour sampling and a shader hook upstream to GPUI? | M11 |
| Q4 | Pace emulation from the audio clock or from display vsync? (Audio-driven with dynamic rate control is the report's default.) | M3 |
| Q5 | Bundle SameBoy's Expat-licensed open boot ROMs so the GB boots with an animation? | M2 |
| Q6 | Write the optional CHIP-8 warm-up (M0) or start straight on the SM83? | Now |
