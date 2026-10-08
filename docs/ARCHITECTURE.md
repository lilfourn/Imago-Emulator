# ds-emulator — Architecture

How the workspace fits together. The *why* behind the big choices lives in
[decisions/](decisions/); the *what* and *when* in [PRD.md](PRD.md) and
[ROADMAP.md](ROADMAP.md).

## Crates

```
                 emu-app (GPUI, binary: ds-emulator)     emu-headless (binary)
                    │        │                                 │
                    │  emu-state, emu-cheats                   │
                    │        │                                 │
                    └──── emu-cores ───────────────────────────┘
                     ┌───────┼────────┐
                  emu-gb  emu-gba  emu-nds
                     │       ├────────┤
                     │    emu-arm  emu-sched (as cores adopt it)
                     │    emu-gba-cart (the GBA cartridge)
                     └───────┴────────┘
                          emu-core
```

| Crate | One job |
|---|---|
| `emu-core` | The contract between cores and frontends: the `Core` trait and plain-data types. No dependencies beyond `thiserror`. |
| `emu-sched` | A priority queue of cycle-timestamped events. |
| `emu-state` | The save-state container format. |
| `emu-arm` | One ARM interpreter, generic over the CPU model: the ARM7TDMI (ARMv4T) for the GBA and the DS's ARM7, and the ARM946E-S (ARMv5TE, CP15, TCM) for the DS's ARM9. Also its disassembler and a bus wrapper for watchpoints. |
| `emu-psg` | The Game Boy family's four-channel sound generator, shared by the GB and GBA cores; each system mixes it. |
| `emu-gba-cart` | The GBA cartridge: ROM, SRAM, Flash and EEPROM saves, and the GPIO clock. Shared by the GBA core and the DS's GBA slot ([decision 0006](decisions/0006-gba-slot.md)); depends only on `serde`. |
| `emu-gb` | Game Boy: SM83, PPU, APU, timer, cartridges. Ticks every component on each bus access ([decision 0002](decisions/0002-game-boy-timing.md)). |
| `emu-gba` | Game Boy Advance: memory map and wait states, PPU, APU with DMA FIFOs, DMA, timers, HLE BIOS, the cartridge (`emu-gba-cart`), serial port, and the linked pair. Event-driven by `emu-sched`. |
| `emu-nds` | Nintendo DS: two CPUs in lockstep slices over shared hardware (`hardware.rs`), each with its own memory and I/O map; VRAM banking, both 2D engines, the 3D engine, sound, DMA, timers, IPC, the maths unit, the game card, the GBA slot, SPI devices, RTC, display capture, direct boot and an HLE BIOS for both CPUs. |
| `emu-cores` | Maps a ROM to its system and constructs the core. The only crate that knows every system. |
| `emu-cheats` | Reads cheat lists, one module per cheat device (Action Replay DS, GameShark Advance, Pro Action Replay v3, CodeBreaker, GameShark, Game Genie, raw patches), and applies them through a small `Memory` trait: `peek`, `poke`, `poke_bank` for Game Boy banks and `poke_io_16` for GBA registers. ROM patches don't run; `rom_patches` hands them back as `emu_core::RomPatch`es. Knows code formats, not cores. |
| `emu-headless` | Runs a core without a window; prints `key=value` lines. |
| `emu-app` | The GPUI desktop app. The only crate that knows about GPUI. |

Rules that keep this shape:

- Cores depend on `emu-core`, never on each other or on a frontend.
- Sharing stops at self-contained chips: the CPU (`emu-arm`), the
  scheduler, the GB/GBA sound generator (`emu-psg`, the same silicon in
  both), and the GBA cartridge (`emu-gba-cart`, the same cartridge in a GBA
  or a DS's GBA slot). Shared system code would tangle timing fixes across
  systems.
- GPUI types never leave `emu-app`. If GPUI is replaced, only that crate
  changes.

## The core contract

```rust
pub trait Core: Send {
    fn info(&self) -> CoreInfo;                       // system, screens, timing, audio rate
    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>); // one per player
    fn screens(&self) -> &[Frame];                     // DS: [top, bottom]
    fn set_render_scale(&mut self, scale: u32);        // DS: 3D at N× resolution, display only
    fn reset(&mut self);
    fn save_state(&self, out: &mut Vec<u8>);
    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError>;
    fn backup_generation(&self, cartridge: usize) -> Option<u64>; // battery-save write counter
    fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>);   // .sav bytes
    fn peek(&self, player: usize, address: u32) -> Option<u8>;  // plain RAM only
    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool;
    fn poke_bank(&mut self, player: usize, bank: u8, address: u32, value: u8) -> bool; // GB banks
    fn poke_io_16(&mut self, player: usize, address: u32, value: u16) -> bool; // GBA registers
    fn set_rom_patches(&mut self, player: usize, patches: &[RomPatch]); // cheats' ROM patches
    fn memory(&self, region: MemoryRegion) -> Option<&[u8]>;
    fn debugger(&mut self) -> Option<&mut dyn Debugger>; // see "Debugging" below
}
```

- **Construction is per system** (`emu_gba::Gba::new(&Media)`), so a core
  always has a game loaded. `Media` carries the ROM, the battery save, any
  user-dumped BIOS/firmware, the RTC start time, and any cartridge in a
  second slot (the DS's GBA slot, [decision 0006](decisions/0006-gba-slot.md)).
- **Players**: `CoreInfo::players` is 1, or 2 for a linked pair
  ([decision 0004](decisions/0004-linked-cores.md)). Each player has an
  input and a cartridge; screens split evenly between players.
- **Battery saves are numbered by cartridge**: player `i`'s cartridge is
  `i`, and a DS's GBA slot cartridge is 1. Each keeps its own `.sav`.
- **Input is per frame and per player**: a 12-bit button mask (bit order follows the GBA
  `KEYINPUT` register, then DS X/Y) plus an optional touch point in
  bottom-screen pixels and a microphone loudness, 0 to 255 (DS).
- **Pixels** are `u32` `0xFFRRGGBB`. On little-endian hosts that is BGRA in
  memory, which GPUI and Metal upload without conversion.
- **Audio** is interleaved `i16` stereo at the core's own rate; the frontend
  resamples.
- **Determinism**: cores never read the clock, randomness or files. Same
  media and inputs give the same frames and state bytes. Rewind, netplay and
  frame-CRC tests depend on it.
- **ROM patches** are configuration, like the ROM itself: a core keeps the
  set it was last given, applies it to every ROM read, leaves it out of
  save states and keeps it through `load_state` and `reset`.
- **The render scale** is configuration too: after `set_render_scale(n)`
  a DS core's frames are n times wider and taller, 3D drawn at that
  resolution. Touch points and layouts stay in `CoreInfo::screens` pixels,
  and emulation, audio and state bytes are the same at every scale.

## Debugging

`emu_core::debug` is the debugger's half of the contract (PRD UI-12).
Every core returns itself from `Core::debugger`:

```rust
pub trait Debugger {
    fn cpus(&self) -> Vec<CpuInfo>;                   // GB: SM83; GBA: ARM7TDMI; DS: ARM9, ARM7
    fn registers(&self, cpu: usize) -> Vec<Register>; // names and values
    fn status(&self, cpu: usize) -> String;           // flags, mode, halted
    fn pc(&self, cpu: usize) -> u32;                  // the next instruction
    fn read(&self, cpu: usize, address: u32) -> Option<u8>;    // no side effects
    fn write(&mut self, cpu: usize, address: u32, value: u8) -> bool; // memory, not ROM or I/O
    fn disassemble(&self, cpu: usize, address: u32) -> Instruction;
    fn io_registers(&self, cpu: usize) -> Vec<IoRegister>;
    fn events(&self, cpu: usize) -> Vec<ScheduledEvent>;       // the scheduler's queue
    fn graphics(&self, cpu: usize, request: &GraphicsRequest) -> Option<Graphics>;
    fn run_until(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>, stops: &Stops)
        -> RunOutcome;                                // the frame, stopping mid-way
}
```

- **CPUs are numbers**: the DS's ARM9 is 0 and its ARM7 1; in a linked
  pair player 0's CPU is 0 and player 1's is 1. Each `CpuInfo` says whose
  player it is, so the app debugs the player the keyboard controls.
- **Two ways to run a frame.** The app calls `run_frame` while nothing
  could stop it, so breakpoints cost nothing until one is set. With
  breakpoints, watchpoints or a step pending it calls `run_until`, which
  runs the same frame with `Stops` checked after every instruction, and
  returns at the first that applies. The next call, or `run_frame`,
  carries on mid-frame. Each core writes its frame loop once, over a step
  closure (GB, GBA) or a `Stepper` type parameter (the DS's slice loop);
  for plain runs these inline back to the old loop. Pokémon White's
  headless speed and CPU trace were unchanged by it.
- **Breakpoints** are tested after each instruction against the next
  one's address, so a run that starts on one runs it instead of stopping
  at once. A step is "stop after one instruction of this CPU"; stepping
  over a call is a breakpoint after it.
- **Watchpoints** see the data accesses the CPU makes itself: the SM83
  runs on a watching bus (its opcode fetches go through `Bus::fetch`, so
  they never count), and the ARM CPUs on `emu_arm::Watched`, which an
  ARM9 also tells about TCM accesses that never reach the bus
  (`Bus::tcm_data`, a no-op elsewhere). DMA, the Game Boy's OAM DMA and
  HDMA, sound, display capture and the HLE BIOS's own accesses are not
  seen.
- **Stopping mid-frame keeps timing**: a linked pair remembers the
  frame's and slice's end, and the DS the end of the slice it stopped in,
  so a stopped frame finishes on the same slices as one that never
  stopped. Tests compare the save states.
- **Reads have no side effects**: each core peeks its whole map,
  including ROM and the I/O registers whose reads change nothing (the
  DS's IPC FIFO and game card data ports read as unreadable). The I/O
  view reads a table of GBATEK and Pan Docs names per CPU
  (`io_names.rs`); the graphics viewers (`graphics.rs`) decode tiles,
  maps, sprites and palettes from the PPU's memory into `Frame`s with
  notes.

## Threads in the app

```
 main thread (GPUI)                         emulation thread
 ──────────────────                         ────────────────
 key/mouse events ──► Command (mpsc) ─────► apply between frames
                                            run_frame()
                                            apply cheats (Core::peek/poke)
 render(): copy latest frames ◄── Mutex ─── publish screens
           pre-scale to BGRA                 │
           img() per screen                  │
 cx.notify() ◄─── Event (async channel) ◄───┘ coalesced "frame ready", notices
                                            flush battery save every 1 s
                                            sleep to the next frame deadline
```

- File I/O for loading runs on GPUI's background executor, never on the
  main thread.
- The emulation thread owns the core outright; nothing else touches it.
- Dropping the `Emulator` handle closes its command channel. The thread
  finishes the frame, flushes the battery save, and is joined. The view
  drops it on app quit for the same reason.
- Frame pacing is a sleep to the next deadline. Sound stays in sync through
  a ring buffer and a resampler that nudges its ratio by up to ±0.5%
  ([decision 0003](decisions/0003-frame-pacing.md)). The `cpal` stream lives
  on the UI thread; the emulation thread owns the producer end.
- A gamepad thread polls `gilrs` and sends plain `PadEvent`s ("South
  pressed") to the view, which maps them through the same bindings as the
  keyboard. `controls.rs` combines every held key and control, so releasing
  one never cancels another that holds the same button. The right stick
  also sends pointer motion, which moves a touch cursor kept in
  touchscreen pixels (`cursor.rs`); the mouse and the cursor touch
  independently, and the mouse wins while both do.
- Gamepad events pass through `chords.rs` before the bindings: while the
  hotkey modifier (Select) is down, a hotkey control dispatches the same
  GPUI action as its menu item, and neither control reaches the game.
  Bindings are the user's file with the game's `game.bindings.toml`
  laid over it, re-read whenever a game starts.
- Turbo buttons cross to the emulation thread on their own
  (`Command::SetTurbo`), not through the latch, whose minimum hold would
  stretch short presses. `turbo.rs` pulses them by counting frames run,
  so turbo is deterministic and keeps pace with fast-forward.
- Cheats come from `game.cht`, which the view re-reads whenever the
  window regains focus and sends as a `Command`. They run after each
  frame: frames end at VBlank, where cheat devices hooked in, so button
  codes see the frame's own input. ROM patches instead go to the core
  when the list arrives (`Core::set_rom_patches`), so a cheat switched
  off or deleted takes its patches with it. DS cheats work on main RAM
  directly and read KEYINPUT, EXTKEYIN and the ARM7's timers. melonDS
  instead runs them on the ARM7's bus at its VBlank interrupt, as the
  real Action Replay does; that reaches the same main RAM and registers.
- The debugger window (`debugger_window.rs`) sends plain
  `Command::Debug` requests: what to show, the breakpoint and watchpoint
  lists, pause, continue, step, step over, next frame, and memory writes.
  While it is open the thread copies a `Snapshot` out of the core's
  `Debugger` after every frame and every debugger command (registers,
  disassembly, 256 bytes of memory, and only the panel shown) and wakes
  the window with its own coalesced event, so it updates even while the
  game's window is hidden. A stop mid-frame pauses before cheats, input
  latches, sound and rewind, which wait for the frame's end.
- The emulation thread records a save state after every frame into the
  rewind history (`rewind.rs`): newest state whole, older ones as
  LZ4-compressed XOR deltas, within 64 MiB.

## Video path

GPUI draws images with linear filtering and has no nearest-neighbour option.
`emu-app/src/video.rs` enlarges each frame by the largest integer factor that
fits the on-screen size in device pixels (capped at 6×), and lets GPUI's
linear filter cover the remaining fraction. That is the "sharp bilinear"
technique: crisp pixels without uneven pixel widths at non-integer sizes.

Filters are CPU passes over frames before they become images
([decision 0005](decisions/0005-video-filters-on-the-cpu.md)). Today the
only one is GBC/GBA colour correction (`color.rs`), a 32,768-entry lookup
table from each 15-bit console colour to its corrected pixel.

DS 3D upscaling is the core's job, as that decision planned. At a render
scale above 1, the DS draws each 3D frame twice: at the native
resolution, which display capture can put in VRAM for the game, and
larger, in 16 horizontal bands on scoped threads. Engine A copies each
line that shows 3D, with the registers that composed it, and at VBlank
composes those lines again around the larger 3D frame, a few lines to a
thread; everything else is enlarged. The app draws the larger frames like
any others and pre-scales them less.

GPUI cannot rotate images either, so rotated layouts turn each screen on
the CPU too (`video::turn`). The touch mapping in `layout.rs` uses the same
pixel mapping, so a touch lands on the pixel drawn under it.

Each new frame becomes a fresh `RenderImage`, and the previous one is
removed from GPUI's atlas with `window.drop_image`. GPUI 0.2.2 only reclaims
atlas space when every image in a texture is dropped, so the app keeps no
other long-lived polychrome images. Memory measured flat over a 20 s run.

## On-disk files

| File | Location | Format |
|---|---|---|
| Battery save | next to the ROM, `game.sav` | Raw backup bytes in the de facto layout for the system |
| Save state | next to the ROM, `game.ss1` | `emu-state` container |
| Cheats | next to the ROM, `game.cht` | Plain text, see the `emu-cheats` crate docs |
| Key bindings | `~/Library/Application Support/ds-emulator/bindings.toml` | TOML, see `emu-app/src/bindings.rs` |
| One game's bindings | next to the ROM, `game.bindings.toml` | The same TOML, laid over the key bindings |
| Layout, speeds, 3D resolution | `~/Library/Application Support/ds-emulator/settings.toml` | TOML written by the app, see `emu-app/src/settings.rs` |

Writes go to `name.tmp` first and are renamed over the target.

## Save-state container

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `EMUSTATE` |
| 8 | 2 | container version (1) |
| 10 | 1 | system tag (1 GB, 2 GBA, 3 DS) |
| 12 | 2 | core state version |
| 16 | 4 | ROM CRC-32 |
| 20 | 4 | payload length *n* |
| 24 | *n* | core payload |
| 24 + *n* | 4 | CRC-32 of everything before it |

The app refuses a state from another system, another ROM, or another core
state version, and says which.

## Scheduling inside a core

`emu-sched` keeps events in a binary heap ordered by (cycle, insertion
order). A core's main loop:

1. Ask `cycles_until_next()` and run the CPU at most that far (for the DS,
   in interleaved ARM9/ARM7 slices).
2. `advance()` by the cycles actually run.
3. `pop_due()` until empty, rescheduling repeating events from their
   *scheduled* time so overshoot never accumulates.

`pending()` and `restore()` put the queue into save states.

## Testing

| Layer | Where | Run |
|---|---|---|
| Unit tests | `#[cfg(test)]` in each module | `cargo test --workspace` |
| Determinism | headless runner, run twice and diff | M1 test-ROM script |
| Test ROMs | `tests/test_roms.rs` in `emu-gb`, `emu-gba` and `emu-nds`, each suite with a passing list (TOOL-2) | `cargo test`, in CI (the mGBA suite only locally) |
| Trace diffing | trace logger vs. mGBA/SameBoy/melonDS (TOOL-3) | by hand while debugging |
| Pokémon checkpoints | your own dumps, by hand | at each milestone |

## Adding a core

1. Create `crates/emu-<system>` depending on `emu-core`.
2. Implement `Core`, with a `new(&Media) -> Result<Self, LoadError>` and a
   `pub const INFO: CoreInfo`.
3. Add a `System` variant in `emu-core`, and the extension, `info` and
   `create` arms in `emu-cores`.
4. Add header-validation and save-state round-trip tests.
