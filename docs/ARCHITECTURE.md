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
                     └───────┴────────┘
                          emu-core
```

| Crate | One job |
|---|---|
| `emu-core` | The contract between cores and frontends: the `Core` trait and plain-data types. No dependencies beyond `thiserror`. |
| `emu-sched` | A priority queue of cycle-timestamped events. |
| `emu-state` | The save-state container format. |
| `emu-arm` | The ARM7TDMI interpreter (ARMv4T) shared by the GBA and both DS CPUs; ARMv5TE arrives with the DS. |
| `emu-psg` | The Game Boy family's four-channel sound generator, shared by the GB and GBA cores; each system mixes it. |
| `emu-gb` | Game Boy: SM83, PPU, APU, timer, cartridges. Ticks every component on each bus access ([decision 0002](decisions/0002-game-boy-timing.md)). |
| `emu-gba` | Game Boy Advance: memory map and wait states, PPU, APU with DMA FIFOs, DMA, timers, HLE BIOS, cartridge saves and RTC, serial port, and the linked pair. Event-driven by `emu-sched`. |
| `emu-nds` | Nintendo DS. Today: header validation and a test pattern. |
| `emu-cores` | Maps a ROM to its system and constructs the core. The only crate that knows every system. |
| `emu-cheats` | Parses cheat lists into memory operations and applies them through a two-method `Memory` trait. Knows code formats, not cores. |
| `emu-headless` | Runs a core without a window; prints `key=value` lines. |
| `emu-app` | The GPUI desktop app. The only crate that knows about GPUI. |

Rules that keep this shape:

- Cores depend on `emu-core`, never on each other or on a frontend.
- Sharing stops at self-contained chips: the CPU (`emu-arm`), the
  scheduler, and the GB/GBA sound generator (`emu-psg`, the same silicon in
  both). Shared system code would tangle timing fixes across systems.
- GPUI types never leave `emu-app`. If GPUI is replaced, only that crate
  changes.

## The core contract

```rust
pub trait Core: Send {
    fn info(&self) -> CoreInfo;                       // system, screens, timing, audio rate
    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>); // one per player
    fn screens(&self) -> &[Frame];                     // DS: [top, bottom]
    fn reset(&mut self);
    fn save_state(&self, out: &mut Vec<u8>);
    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError>;
    fn backup_generation(&self, player: usize) -> Option<u64>; // battery-save write counter
    fn save_backup(&self, player: usize, out: &mut Vec<u8>);   // .sav bytes
    fn peek(&self, player: usize, address: u32) -> Option<u8>;  // plain RAM only
    fn poke(&mut self, player: usize, address: u32, value: u8) -> bool;
    fn memory(&self, region: MemoryRegion) -> Option<&[u8]>;
}
```

- **Construction is per system** (`emu_gba::Gba::new(&Media)`), so a core
  always has a game loaded. `Media` carries the ROM, the battery save, any
  user-dumped BIOS/firmware, and the RTC start time.
- **Players**: `CoreInfo::players` is 1, or 2 for a linked pair
  ([decision 0004](decisions/0004-linked-cores.md)). Each player has an
  input and a cartridge; screens split evenly between players.
- **Input is per frame and per player**: a 12-bit button mask (bit order follows the GBA
  `KEYINPUT` register, then DS X/Y) plus an optional touch point in
  bottom-screen pixels.
- **Pixels** are `u32` `0xFFRRGGBB`. On little-endian hosts that is BGRA in
  memory, which GPUI and Metal upload without conversion.
- **Audio** is interleaved `i16` stereo at the core's own rate; the frontend
  resamples.
- **Determinism**: cores never read the clock, randomness or files. Same
  media and inputs give the same frames and state bytes. Rewind, netplay and
  frame-CRC tests depend on it.

## Threads in the app

```
 main thread (GPUI)                         emulation thread
 ──────────────────                         ────────────────
 key/mouse events ──► Command (mpsc) ─────► apply between frames
                                            apply cheats (Core::poke)
                                            run_frame()
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
  one never cancels another that holds the same button.
- Cheats come from `game.cht`, which the view re-reads whenever the
  window regains focus and sends as a `Command`. The memory viewer asks
  the thread to copy a span of RAM (through `Core::peek`) into the shared
  latest-frame slot after every frame.
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
| Test ROMs | headless runner + manifest (TOOL-2) | M1 onward, in CI |
| Trace diffing | trace logger vs. mGBA/SameBoy/melonDS (TOOL-3) | by hand while debugging |
| Pokémon checkpoints | your own dumps, by hand | at each milestone |

## Adding a core

1. Create `crates/emu-<system>` depending on `emu-core`.
2. Implement `Core`, with a `new(&Media) -> Result<Self, LoadError>` and a
   `pub const INFO: CoreInfo`.
3. Add a `System` variant in `emu-core`, and the extension, `info` and
   `create` arms in `emu-cores`.
4. Add header-validation and save-state round-trip tests.
