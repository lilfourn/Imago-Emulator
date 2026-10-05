# ds-emulator — Architecture

How the workspace fits together. The *why* behind the big choices lives in
[decisions/](decisions/); the *what* and *when* in [PRD.md](PRD.md) and
[ROADMAP.md](ROADMAP.md).

## Crates

```
                 emu-app (GPUI, binary: ds-emulator)     emu-headless (binary)
                    │        │                                 │
                    │     emu-state                            │
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
| `emu-arm` | The ARM interpreter shared by the GBA and both DS CPUs. Empty until M7. |
| `emu-gb`, `emu-gba`, `emu-nds` | One system each. Today: header validation and a test pattern. |
| `emu-cores` | Maps a ROM to its system and constructs the core. The only crate that knows every system. |
| `emu-headless` | Runs a core without a window; prints `key=value` lines. |
| `emu-app` | The GPUI desktop app. The only crate that knows about GPUI. |

Rules that keep this shape:

- Cores depend on `emu-core`, never on each other or on a frontend.
- Sharing between the GBA and DS stops at the CPU (`emu-arm`) and the
  scheduler. Shared system code would tangle timing fixes across systems.
- GPUI types never leave `emu-app`. If GPUI is replaced, only that crate
  changes.

## The core contract

```rust
pub trait Core: Send {
    fn info(&self) -> CoreInfo;                       // system, screens, timing, audio rate
    fn run_frame(&mut self, input: &Input, audio: &mut Vec<StereoSample>);
    fn screens(&self) -> &[Frame];                     // DS: [top, bottom]
    fn reset(&mut self);
    fn save_state(&self, out: &mut Vec<u8>);
    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError>;
    fn backup(&self) -> Option<Backup<'_>>;            // battery save + write counter
    fn memory(&self, region: MemoryRegion) -> Option<&[u8]>;
}
```

- **Construction is per system** (`emu_gba::Gba::new(&Media)`), so a core
  always has a game loaded. `Media` carries the ROM, the battery save, any
  user-dumped BIOS/firmware, and the RTC start time.
- **Input is per frame**: a 12-bit button mask (bit order follows the GBA
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
- Frame pacing today is a sleep to the next deadline. Audio-driven pacing
  arrives with sound (M3).

## Video path

GPUI draws images with linear filtering and has no nearest-neighbour option.
`emu-app/src/video.rs` enlarges each frame by the largest integer factor that
fits the on-screen size in device pixels (capped at 6×), and lets GPUI's
linear filter cover the remaining fraction. That is the "sharp bilinear"
technique: crisp pixels without uneven pixel widths at non-integer sizes.

Each new frame becomes a fresh `RenderImage`, and the previous one is
removed from GPUI's atlas with `window.drop_image`. GPUI 0.2.2 only reclaims
atlas space when every image in a texture is dropped, so the app keeps no
other long-lived polychrome images. Memory measured flat over a 20 s run.
A shader chain would need its own Metal or wgpu path (PRD open question Q3).

## On-disk files

| File | Location | Format |
|---|---|---|
| Battery save | next to the ROM, `game.sav` | Raw backup bytes in the de facto layout for the system |
| Save state | next to the ROM, `game.ss1` | `emu-state` container |
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
