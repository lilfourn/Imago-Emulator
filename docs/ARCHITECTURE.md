# ds-emulator — Architecture

How the workspace fits together. The *why* behind the big choices lives in
[decisions/](decisions/); the *what* and *when* in [PRD.md](PRD.md) and
[ROADMAP.md](ROADMAP.md).

## Crates

```
                 emu-app (GPUI, binary: ds-emulator)     emu-headless (binary)
                    │        │                                 │
      emu-patch ◄───┤  emu-state, emu-cheats, emu-script ◄─────┤
      emu-guide ◄───┤        │                                 │
                    │        │                                 │
                    └──── emu-cores ───────────────────────────┘
                     ┌───────┼────────┬──────────┬──────────────┐
                  emu-gb  emu-gba  emu-nds    emu-mkxp      emu-3ds
                     │       ├────────┤     (RPG Maker)  (Azahar, 3DS)
                     │    emu-arm  emu-sched (as cores adopt it)
                     │    emu-gba-cart (the GBA cartridge)
                     └───────┴────────┴──────────┴──────────────┘
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
| `emu-gb` | Game Boy: SM83, PPU, APU, timer, cartridges, and the Game Boy Color's palettes for games made for the original Game Boy (SameBoy's table, MIT). Ticks every component on each bus access ([decision 0002](decisions/0002-game-boy-timing.md)). |
| `emu-gba` | Game Boy Advance: memory map, wait states and the Game Pak prefetch buffer, PPU, APU with DMA FIFOs, DMA, timers, the interrupt controller, HLE BIOS, the cartridge (`emu-gba-cart`), serial port, and the linked pair. Event-driven by `emu-sched`. |
| `emu-nds` | Nintendo DS: two CPUs in lockstep slices over shared hardware (`hardware.rs`), each with its own memory and I/O map; VRAM banking, both 2D engines, the 3D engine, sound, DMA, timers, IPC, the maths unit, the game card, the GBA slot, SPI devices, RTC, display capture, direct boot and an HLE BIOS for both CPUs; the Wi-Fi chip (`wifi/`), and two consoles in wireless range as one linked core ([decision 0007](decisions/0007-ds-local-wireless.md)). |
| `emu-mkxp` | RPG Maker games, such as Pokémon Essentials fan games, which mkxp-z plays. Runs the game's own Mac player app, made hidden, in a process of its own, and talks to a bridge script in it over a Unix socket: frames and buttons a step at a time, and save states that are the game's own saves. Captures its sound through OpenAL Soft's WAV writer and a FIFO, and hands it on as it comes. The player takes the app's scheduling role, so macOS's Game Mode does not throttle it ([decision 0008](decisions/0008-rpg-maker-games.md)). |
| `emu-3ds` | Nintendo 3DS games on Azahar's libretro core, which it downloads at first use with MoltenVK, at pinned versions checked against pinned digests (`install.rs`). Runs the core in a helper process, the app's own executable started with `--3ds-helper` (`host.rs`): it gives the core a Vulkan device through MoltenVK and reads each frame back (`vulkan.rs`), cuts the picture into the two screens and answers the app a frame at a time over its stdin and stdout (`link.rs`). Reads game files' headers for titles, encryption and extdata IDs (`header.rs`), and writes out the executable an unencrypted installable title (`.cia`) holds, which the core opens in its place (`cia.rs`). The helper takes the app's scheduling role, as the RPG Maker player does ([decision 0011](decisions/0011-3ds-games.md)). |
| `emu-cores` | Maps a ROM to its system and constructs the core. The only crate that knows every system. |
| `emu-cheats` | Reads cheat lists, one module per cheat device (Action Replay DS, GameShark Advance, Pro Action Replay v3, CodeBreaker, GameShark, Game Genie, raw patches), and applies them through a small `Memory` trait: `peek`, `poke`, `poke_bank` for Game Boy banks and `poke_io_16` for GBA registers. ROM patches don't run; `rom_patches` hands them back as `emu_core::RomPatch`es. Knows code formats, not cores. |
| `emu-patch` | Applies ROM-hack patches: IPS, UPS, BPS and xdelta's VCDIFF, with its LZMA compression, checked by each format's own checksums and held to a size limit before anything is allocated. Bytes in, bytes out: knows formats, not files ([decision 0009](decisions/0009-rom-hack-patches.md)). |
| `emu-script` | Runs a Lua script against a core between frames (Lua 5.4 through `mlua`, built from its bundled sources). Reads and writes memory through the debugger interface; buttons for the next frame, drawing, log lines, state-slot and pause requests come back as plain data (`Output`) for the frontend to act on. Sandboxed: no files, clock or modules, with memory and instruction limits. Knows no frontend. See [scripting.md](scripting.md). |
| `emu-guide` | Guide, the chat about the game being played ([PRD](PRD-game-chat.md)): connecting an OpenRouter account by OAuth PKCE through the browser and a loopback port, as Imago does; the key in a 0600 file, with `OPENROUTER_API_KEY` over it; OpenRouter's API through `curl`, the key and the request on its standard input; answers streamed as server-sent events; the source map (`sources.toml`) and the system prompt; Markdown parsed block by block as it streams; each game's chats and screenshots on disk. Knows no window. The panel itself is `emu-app`'s `guide` module, drawn as a cached view beside the screens ([decision 0014](decisions/0014-guide-through-openrouter.md)). |
| `emu-headless` | Runs a core without a window, or an RPG Maker game in a hidden player of its own; prints `key=value` lines. |
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
    fn set_texture_upscaling(&mut self, upscaling: TextureUpscaling); // 3DS: textures, display only
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
    fn try_save_state(&self, out: &mut Vec<u8>) -> Result<(), String>; // save_state, or why not
    fn saves_every_frame(&self) -> bool;               // false: no rewind, states only when asked
    fn checkpoints_while_playing(&self) -> bool;       // checkpoint every minute, not just on leaving
    fn ended(&self) -> Option<String>;                 // why a game in another process stopped
}
```

- **Construction is per system** (`emu_gba::Gba::new(&Media)`), so a core
  always has a game loaded. `Media` carries the ROM, the battery save, any
  user-dumped BIOS/firmware, the RTC start time, any cartridge in a
  second slot (the DS's GBA slot, [decision 0006](decisions/0006-gba-slot.md)),
  and how a Game Boy shows games made for the original Game Boy: in grey,
  or on a Game Boy Color in its colours.
- **Players**: `CoreInfo::players` is 1, or 2 for a linked pair
  ([decision 0004](decisions/0004-linked-cores.md)). Each player has an
  input and a cartridge; screens split evenly between players, so two
  linked DSes have four: each console's top screen, then its bottom one.
  `touch_screen` counts within a console, and each DS has its own.
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
  frame-CRC tests depend on it. The one exception is `emu-mkxp`, whose game
  runs in another process: its states are the game's own saves, made only
  when asked (`saves_every_frame` is false), so it has no rewind, and it
  refuses a state where the game cannot save, as in a battle. Its
  checkpoint is asked for as the player leaves and every minute it plays.
  `emu-3ds` is the other: Azahar runs in another process on threads and a
  clock of its own. Its states are the whole console, made anywhere but
  in a few hundred milliseconds, so it has no rewind either, and its
  checkpoint is asked for only as the player leaves
  (`checkpoints_while_playing` is false).
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
  pair player 0's CPU is 0 and player 1's is 1, and in a linked DS pair
  player 0's ARM9 and ARM7 are 0 and 1 and player 1's are 2 and 3. Each
  `CpuInfo` says whose player it is, so the app debugs the player the
  keyboard controls.
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
 main thread (GPUI)                         emulation thread                recording thread
 ──────────────────                         ────────────────                ────────────────
 key/mouse events ──► Command (mpsc) ─────► apply between frames
                                            run_frame()
                                            apply cheats (Core::peek/poke)
                                            run the Lua script, if any
                                            record frame and sound ───────► JPEG, QuickTime
 render(): swap in latest frames ◄─ Mutex ─ publish screens                 (bounded queue)
           correct, turn, then upscale       │
           on the GPU or filter and          │
           pre-scale to BGRA                 │
           img() per screen                  │
 cx.notify() ◄─── Event (async channel) ◄───┘ coalesced "frame ready", notices
                                            serialize for rewind ─────────► rewind thread:
                                            flush battery save every 1 s    XOR, LZ4, history
                                            sleep to the next frame deadline
```

- File I/O for loading runs on GPUI's background executor, never on the
  main thread.
- The emulation thread owns the core outright; nothing else touches it.
  It runs at the user-interactive quality of service, the window's own
  class (`qos.rs`): a thread the app starts would otherwise run at macOS's
  default, behind other interactive work, with its sleeps let slip.
- Dropping the `Emulator` handle closes its command channel. The thread
  finishes the frame, flushes the battery save, and is joined. The view
  drops it on app quit for the same reason.
- Frame pacing is a sleep to the next deadline, taken in naps of three
  quarters of the time left, because macOS wakes a thread up to a quarter
  of its sleep late (`speed::sleep_until`). Fast-forward (2× unless the
  Emulation menu or settings say otherwise) and slow motion move the
  deadlines closer or further apart. Sound stays in sync through
  a ring buffer and a resampler that nudges its ratio by up to ±0.5%
  ([decision 0003](decisions/0003-frame-pacing.md)). The `cpal` stream lives
  on the UI thread; the emulation thread owns the producer end, but for an
  RPG Maker game, whose sound runs on its player's clock: the thread that
  reads that sound owns it and plays the sound as it comes, at normal
  speed, rather than after each frame (`Loaded::live_sound`).
- Frames keep in step with the display's refreshes (`refresh.rs`): when
  a whole number of refreshes, or of frames to a refresh, is within 0.5%
  of the frame the speed asks for, frames last exactly that, and each
  frame's start is nudged by up to 1 ms so that a frame of the usual length
  is ready a margin before the refresh that shows it: 2 ms, or more for a
  game whose frames take uneven times, as much as lets the most of its
  last 120 frames show at their own refresh. The view tells the thread the display's refresh
  interval (`Command::SetRefresh`, from AppKit) and when it takes each
  frame (`Latest::taken`); sound plays as much faster as frames then run.
- The emulation thread counts the frames it runs and rewind steps it
  takes (`Latest::frames_shown`); while View > Show FPS is on, the view
  samples the count twice a second (`meter.rs`) for the readout in the
  window's corner.
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
- A Lua script (`scripting.rs`, the `emu-script` crate) runs here too.
  The view reads the file and sends its text; the thread runs its main
  body at once, then after each frame's cheats, lending it the core for
  that call only. What it draws is published with the frame
  (`Latest::overlay`) and painted by the view as GPUI elements over each
  place its screen is shown, clipped to it. The buttons it presses join
  the next frame's input after the latch and turbo, and its state and
  pause requests go through the same paths as the menu's. Its
  `emu.on_exec` callbacks run mid-frame through `Debugger::run_until`
  with the debugger window's stops, and only the window's pause. A
  script error drops the script with a notice. In `emu-headless` the
  same script runs frame by frame, with state slots in memory.
- The debugger window (`debugger_window.rs`) sends plain
  `Command::Debug` requests: what to show, the breakpoint and watchpoint
  lists, pause, continue, step, step over, next frame, and memory writes.
  While it is open the thread copies a `Snapshot` out of the core's
  `Debugger` after every frame and every debugger command (registers,
  disassembly, 256 bytes of memory, and only the panel shown) and wakes
  the window with its own coalesced event, so it updates even while the
  game's window is hidden. A stop mid-frame pauses before cheats, input
  latches, sound and rewind, which wait for the frame's end.
- The emulation thread serializes the core after every frame for the
  rewind history (`rewind.rs`), and a rewind thread
  (`rewind_thread.rs`) does the rest: the newest state whole, older ones
  as LZ4-compressed XOR deltas, within 64 MiB. A state's numbers take as
  many bytes as their values need, so a growing register moves
  everything after it; each 4 KB of the older state is XORed with where
  it went in the newer one, so a Pokémon Platinum delta is about 41 KB
  rather than 2.8 MB, and the history holds about 27 seconds. A DS state is about 6 MB,
  and the XOR and compression took as long as serializing, so the
  emulation thread only serializes, into a buffer the rewind thread
  handed back; if that thread falls two states behind, frames are
  skipped rather than queued. Faster than normal speed, a frame is
  recorded only about once per normal frame's time, so fast-forward
  pays for some 60 snapshots a second, not one per frame, and rewinding
  goes back through it as fast as it went by. Stepping back asks the
  rewind thread for each state and waits for it.
- Screenshots and recordings take the core's own frames on the emulation
  thread, never the coalesced latest frame the view draws, which can skip
  frames. A screenshot copies the screens between frames and leaves the
  PNG encoding and writing to a short-lived thread. A recording
  (`recorder.rs`) hands every frame that runs to its own thread, with
  the sound the frame made, which the audio sink resamples and may drop
  but the recording keeps whole. Its queue holds 16 frames; past that
  the emulation waits for the writer, so no frame is lost even at
  unlimited fast-forward. Frames that never run, while paused or
  rewinding, are not in the movie.

## Video path

GPUI draws images with linear filtering and has no nearest-neighbour option
or shader hook. Each screen is placed on whole device pixels
(`Rect::snapped`), then drawn as one of View > Picture's looks
(`picture.rs`). Every new frame goes through, in this order:
1. GBC/GBA colour correction (`color.rs`), a 32,768-entry lookup table
   from each 15-bit console colour to its corrected pixel;
2. rotation (`video::turn`, below);
3. for every look but the shading filters, the GPU (`gpu_upscale.rs`,
   [decision 0013](decisions/0013-upscaling-on-the-gpu.md)): the frame
   goes up to a Metal texture and is drawn at the placement's exact size
   in device pixels, then read back as the bytes of the GPUI image. Sharp
   Pixels (`shaders/sharp.metal`), the default for pixel-art games, keeps
   each console pixel a crisp, even square at any size: each output pixel
   takes the console pixel under its middle, except the one across an edge,
   which mixes the two in linear light by how much of it each covers.
   Smooth is MetalFX's spatial scaler, Auto's fit for the 3DS. Rounded
   Pixels is OmniScale, which reshapes pixel art, for those who choose it.
   Smooth and Rounded Pixels only enlarge; where they would not, Sharp
   Pixels draws the frame. All the screens of a frame together stay
   within 6 million pixels (`picture::MAX_UPSCALED_PIXELS`); past that
   they are drawn smaller and GPUI's linear filter enlarges the rest. On
   this M5 Max that took 1.1–1.4 ms a frame for a GB screen or two DS
   screens of about 1.5 million pixels in all. The GPU starts on a
   background thread at launch, as compiling the shader takes about
   130 ms; until then, and on a Mac without Metal, screens take the CPU
   path;
4. otherwise the CPU filters ([decision 0005](decisions/0005-video-filters-on-the-cpu.md),
   `filter.rs`). LCD grid and scanlines take over the integer pre-scale
   and darken part of each console pixel's square of device pixels.
   Scale2x and Scale3x, which stand in for Rounded Pixels without Metal,
   enlarge two or three times, rounding diagonal edges, and leave the rest
   of the pre-scale to `video.rs`. Their masks turn with the screen, and on
   DS frames drawn at a higher 3D resolution they follow console pixels
   (the scalers skip those frames). Each costs at most about 0.7 ms per DS
   screen at 6×. Sharp Pixels without the GPU is the plain pre-scale:
   `video.rs` enlarges each frame by the largest integer factor that fits
   (capped at 6×) and GPUI's linear filter covers the remaining fraction,
   the "sharp bilinear" technique, softer than the GPU's at large sizes.

Sharp Pixels needs nothing special for a DS frame drawn with larger 3D:
the 2D in it is enlarged as squares, which it keeps crisp, beside the
3D's own detail. With Rounded Pixels chosen, OmniScale works on the 2D
only, on
the GPU, in one more pass each way (`shaders/beside_3d.metal`): the frame
is split into console pixels, those whose square of the larger frame is
all one colour carry no 3D detail; those within two console pixels of
one that does are drawn from the larger frame, smooth (MetalFX when it
enlarges by a quarter or more), and the rest by OmniScale from the
console pixels. So menus, text and the touch screen are rebuilt as pixel
art beside the 3D's own detail, with no change to the core. All of a
frame's screens go to the GPU in one command buffer and one wait, which
is most of an upscale's time: in Pokémon Platinum at 4× 3D, both screens'
images took 1.6 ms a frame in the app.

The 3DS filters the game's own textures only when the player chooses a
picture that changes the art (`Core::set_texture_upscaling`, sent on to
the helper as `TEXTURES`): Azahar's ScaleForce with Smooth, MMPX with
Rounded Pixels, none with Auto, Sharp Pixels and the shading filters. 3DS
games hold pixel art too, such as Omega Ruby's AreaNav map, which
ScaleForce blurs.

Match Display, the default 3D resolution (`picture::matched_resolution`),
draws DS and 3DS 3D about as large as the screens are shown: for each
screen, its device pixels over its console pixels, rounded up unless a
quarter or less above a whole number, at most 4×. It keeps the speed
asked for first (`picture::keep_speed`): every second the view compares
the frames run with the speed asked for, and if the game fell behind by
more than 5% while running, it draws 3D a step smaller, never stepping
back up until the speed asked for or the screens change. In Pokémon
Platinum's overworld the core took 3.75 ms a frame with 3D at 1×, 5.2 ms
at 2× and 7.35 ms at 4× (headless), against 5.6 ms a frame at 3×
fast-forward; with the check, 3× fast-forward ran at 172 frames a second
instead of about 110, while normal speed keeps 4×. Since the speed-ups of
November 2026 it takes about 2.5, 3.4 and 4.2–4.6 ms, so 3× fast-forward
can keep 4× and the check steps down less often. The view sends the
core a new render scale only when the scale changes.

DS 3D upscaling is the core's job, as that decision planned. At a render
scale above 1, the DS draws each 3D frame twice: at the native
resolution, which display capture can put in VRAM for the game, and
larger, in 16 horizontal bands. Both draw on worker threads the
renderer keeps (`gpu3d/render/workers.rs`) while the machine runs on: a
native band is waited for when its first line is needed, the larger
frame at the next VBlank. Engine A copies each
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
| Save state | next to the ROM, `game.ss1`; an RPG Maker game's next to its folder, `Game.ss1` | `emu-state` container |
| Checkpoint: where the player left off, picked up when the game starts | slot 0 of the save states, `game.ss0`, with `game.ss0.saves` beside it | `emu-state` container; the record has a line per save the checkpoint goes with, each battery save and an RPG Maker game's own save files, or a 3DS game's save data and extdata: the file's name and the CRC-32 of its bytes, or `none`. A checkpoint is picked up only while they are unchanged |
| Cheats | next to the ROM, `game.cht` | Plain text, see the `emu-cheats` crate docs |
| Key bindings | `~/Library/Application Support/ds-emulator/bindings.toml` | TOML, see `emu-app/src/bindings.rs` |
| One game's bindings | next to the ROM, `game.bindings.toml` | The same TOML, laid over the key bindings |
| Layout, speeds, 3D resolution, picture | `~/Library/Application Support/ds-emulator/settings.toml` | TOML written by the app, see `emu-app/src/settings.rs` |
| Screenshot | next to the ROM, `game 2026-10-07 at 18.58.12.png` | 8-bit RGB PNG of the screens as the core drew them |
| Recording | next to the ROM, `game 2026-10-07 at 18.58.12.mov` | QuickTime movie: Photo-JPEG video, 16-bit stereo PCM sound, see `emu-app/src/mov.rs` |
| Unpacked game | `~/Library/Application Support/ds-emulator/Games/` | The ROM from a `.zip`, `.7z` or `.rar`, see `emu-app/src/archive.rs` |
| A `.cia`'s executable | `~/Library/Application Support/ds-emulator/Games/<name>.cxi` | Content 0 of the installable title, an NCCH, as it is in the `.cia`, see `emu-3ds/src/cia.rs`; the `.cia`'s states stay beside it |
| Game from the store | `~/Library/Application Support/ds-emulator/Games/Store/<catalog>/<slug>/` | The game's ROM as its catalog has it, unpacked if it came in an archive, see `emu-app/src/store.rs` |
| The store's catalogs and pictures | `~/Library/Caches/ds-emulator/store/` | Each catalog's JSON as fetched, and screenshots as PNGs named by a checksum of their URL |
| Game made from a ROM-hack patch | `~/Library/Application Support/ds-emulator/Hacks/`, named after the patch | The patched ROM, see `emu-app/src/romhack.rs` |
| Randomized game | `~/Library/Application Support/ds-emulator/Hacks/<title> (Random <seed>).<ext>`, with `….<ext>.log` beside it | The ROM FVX wrote and its spoiler log, see `emu-app/src/randomizer.rs` |
| The randomizer | `~/Library/Application Support/ds-emulator/Randomizer/upr-fvx-<version>/` | FVX's Mac release as it ships, with its own Java, see `emu-app/src/randomizer.rs` |
| An RPG Maker game's hidden player | `~/Library/Application Support/ds-emulator/RPG Maker/<CRC-32 of the folder's path>/Player.app` | A copy of the game's player app, signed ad hoc; the bridge script beside it, see `emu-mkxp/src/player.rs` |
| The 3DS core and MoltenVK | `~/Library/Application Support/ds-emulator/3DS/azahar-<version>/`, `…/3DS/moltenvk-<version>/` | Azahar's libretro core and MoltenVK as their releases ship them, with their licences, see `emu-3ds/src/install.rs` |
| The emulated 3DS's own files | `~/Library/Application Support/ds-emulator/3DS/Azahar/` | Azahar's NAND, SD card (where 3DS games keep their own saves, in `title/<id>/data/` and `extdata/<id>/`) and shader caches; the helper's log is `3DS/helper.log`, and `3DS/helper.lock` keeps a second helper off these files until the first has quit |

Writes go to `name.tmp` first and are renamed over the target, except
recordings, which are written as they go and indexed when they stop.
Games unpacked or patched go to `name.partial` instead, locked while
they are written. A `.7z` or `.rar` is unpacked into
`$TMPDIR/ds-emulator-unpack-<pid>-<n>/`, and a store download into
`$TMPDIR/ds-emulator-store-<pid>-<n>/`, each locked by a
`.ds-emulator-lock` file inside while in use. The unpacked game is
moved, not copied, into `Games/`. The app removes its own on quit. At
launch, it removes any folder or `.partial` that no running app holds,
killing any `tar` still writing into it. It also removes `.tmp` files in
`Games/` and `Hacks/`, and patches in `$TMPDIR/ds-emulator-patches/`,
once they are an hour old. See `emu-app/src/leftovers.rs`.

## Save-state container

Version 2, all integers little-endian (`emu-state` also reads version 1,
which has no thumbnail fields: a 24-byte header, the payload, the
checksum):

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `EMUSTATE` |
| 8 | 2 | container version (2) |
| 10 | 1 | system tag (1 GB, 2 GBA, 3 DS, 4 RPG Maker, 5 3DS) |
| 11 | 1 | reserved, 0 |
| 12 | 2 | core state version |
| 14 | 2 | reserved, 0 |
| 16 | 4 | ROM CRC-32 (an RPG Maker game: of its folder's path) |
| 20 | 4 | payload length *n* |
| 24 | 2 | thumbnail width *w*, 0 for none |
| 26 | 2 | thumbnail height *h* |
| 28 | *n* | core payload |
| 28 + *n* | 3*wh* | thumbnail, RGB, row-major |
| end − 4 | 4 | CRC-32 of everything before it |

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
| Determinism | headless runner, run twice and diff; `--digest` adds the sound's and the final save state's CRC-32s, and `emu-headless/tests/bench.rs` checks a run from a state matches one from power-on | M1 test-ROM script |
| Core speed | `emu-headless --time`, from a game's checkpoint with `--state`; before and after a change, with `--digest` to show it changed nothing | by hand |
| ROM-hack patches | `emu-patch/tests/formats.rs` applies patches made by Floating IPS and xdelta3 from made-up ROMs; `scripts/make-patch-fixtures.sh` makes them again | `cargo test` |
| Scripts | `emu-script` unit tests against a test core; `emu-headless/tests/script.rs` runs a script twice on a ROM it builds and diffs the output | `cargo test` |
| Test ROMs | `tests/test_roms.rs` in `emu-gb`, `emu-gba` and `emu-nds`, each suite with a passing list (TOOL-2) | `cargo test`, in CI (the mGBA suite only locally) |
| Trace diffing | trace logger vs. mGBA/SameBoy/melonDS (TOOL-3) | by hand while debugging |
| Pokémon checkpoints | your own dumps, by hand | at each milestone |
| Latency, pacing, CPU | the release app's timing trace (`trace.rs`, `DS_EMULATOR_TRACE`), read by `scripts/macos/bench-report.py` | `scripts/macos/bench.sh GAME` |

## Adding a core

1. Create `crates/emu-<system>` depending on `emu-core`.
2. Implement `Core`, with a `new(&Media) -> Result<Self, LoadError>` and a
   `pub const INFO: CoreInfo`.
3. Add a `System` variant in `emu-core`, and the extension, `info` and
   `create` arms in `emu-cores`.
4. Add header-validation and save-state round-trip tests.
