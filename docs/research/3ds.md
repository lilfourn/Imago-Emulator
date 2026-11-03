# Nintendo 3DS games

Researched 2026-11-03. This covers what it would take for ds-emulator to
play 3DS games, what was tested on this Mac, and a plan.

The PRD rules out writing a 3DS core (section 4). A 3DS emulator needs
high-level emulation of a whole operating system, a GPU shader translator
and, in practice, a JIT. Gen 6–9 support "means hosting an existing core".
That is Phase 5 of the roadmap: a libretro host for an Azahar-derived core.
This research follows that path.

## Summary

- **Azahar is the core to host.** It is Citra's successor, GPL-3.0 and
  active: 2126.2 is the stable release (2026-10-08), and 2127.0 alpha 4
  came out on 2026-11-01. Every release ships an official libretro core,
  including a 7 MB build for macOS arm64.
- **That core runs on this Mac without RetroArch.** A headless host of
  about 240 lines of C booted three homebrew games with the core's software
  renderer. Save states saved and loaded.
- **The software renderer is too slow for commercial games.** A simple 3D
  homebrew ran at 57 fps of 60. Craftus Reloaded's title screen ran at 28.
  Real games need the core's Vulkan renderer, which runs on MoltenVK on a
  Mac. The macOS core has no OpenGL renderer. Providing a Vulkan context is
  the main work, and it hasn't been tried yet.
- **Games must be decrypted.** Azahar refuses encrypted games and ships no
  keys, which fits PRD section 9.5. Players dump their own cartridges as
  decrypted `.3ds`/`.cci` files.
- **Most games need no files from a console.** The core carries
  open-source stand-ins for the system font, Mii data, the country list and
  the bad-word list.

## Emulators

| Emulator | State | For ds-emulator |
|---|---|---|
| Azahar | GPL-3.0, Citra's successor, releases every few weeks, official libretro cores for macOS arm64 and x86_64 | Host it |
| Panda3DS | GPL-3.0, smaller, last release v0.9 (Dec 2024) | Not now |
| AzaharPlus | A fork that loads encrypted games | Out: it needs keys (PRD 9.5) |
| Citra, Lime3DS | Retired; Azahar continues them | None |
| A core of our own | A non-goal (PRD 4) | Only parts, for learning (Phase 5) |

## What the core is

From Azahar 2126.2's source (`src/citra_libretro`) and its macOS arm64
build:

- **Files:** it opens `.3ds`, `.cci`, `.cxi`, `.3dsx`, `.elf`, `.axf` and
  `.app`, plus zstd-compressed `z3dsx`, `zcci` and `zcxi`. It can't open
  `.cia`: a CIA is a title to install, and the core can't install one.
- **Picture:** 60 fps, as one 400×480 picture. The top screen is 400×240,
  and the 320×240 bottom screen sits under it, centred between 40-pixel
  bars. At higher internal resolutions the picture grows; the largest
  the core declares is 7200×4800.
- **Sound:** stereo at 32,728 Hz.
- **Renderers on macOS:** Vulkan, which needs the frontend to provide a
  device (MoltenVK on a Mac), or software. Its OpenGL renderer asks for
  OpenGL 4.3 core, and macOS stops at 4.1, so the macOS build leaves it out.
  The dylib references no `gl*` function.
- **Save states:** through `retro_serialize`, with the variable-size and
  must-initialize quirks. The size changes as the game runs, and there is
  no state until a frame has run. Before saving, it waits up to 5 s for the
  game's pending file and network work.
- **Its own files:** the system NAND, SD card, saves and shader cache go in
  an `Azahar/` folder in the frontend's save directory.
- **Input:** joypad buttons, the circle pad and C stick as analog sticks,
  the touch screen through the pointer or mouse, and motion through the
  sensor interface.
- **Memory:** exposed through `SET_MEMORY_MAPS`, by virtual address.
  `retro_cheat_set` does nothing.

## What was tested

The host loads the core with `dlopen`, forces the software renderer
(`citra_graphics_api=Software`) and leaves every other option at its
default. It taps A for 6 frames every 90, from frame 300, and saves a frame
every 600. After the run it saves a state, runs 120 frames, loads the state
and runs 60 more. The machine is an M5 Max on macOS 27.0.1. The kit is in
`~/Library/Caches/ds-emulator-3ds-research`; its `README.txt` says how to
run it.

| Homebrew (Universal-DB) | Result | State |
|---|---|---|
| ClassiCube | Boots in 0.15 s and stops at "Fetching ClassiCube textures", because the core's network isn't set up. About 2,000 fps on that screen. | 1.1 MB; saved in 538 ms, loaded in 544 ms |
| Craftus Reloaded 0.3 | Title screen with a moving 3D sky: 27.8 fps, 46% of full speed | 0.5 MB; 168 ms, 290 ms |
| 3d-platformer 1.0 | Its first level, in 3D: 56.9 fps, 95%; slowest frame 23 ms | 1.7 MB; 181 ms, 301 ms |

None of the three produced sound. Homebrew's sound library loads DSP
firmware dumped from a console (`sdmc:/3ds/dspfirm.cdc`), and the test had
none. Commercial games use Azahar's built-in audio instead. This wasn't
checked.

What wasn't tested:

- **The Vulkan renderer.** There is no MoltenVK on this Mac yet, and
  providing a Vulkan context is the work described below.
- **Any commercial game.** No dump of the player's own was on hand.
- **Touch, the circle pad and long play.**

## What hosting it needs

### 1. A Vulkan context, through MoltenVK (large; the main risk)

Under libretro's Vulkan contract:

1. The frontend creates the `VkInstance`.
2. The core's negotiation interface creates the device.
3. The frontend hands the core `retro_hw_render_interface_vulkan` (the
   queue, `set_image` and the sync index).
4. After each `retro_run`, the core names the frame's `VkImage`.
5. The frontend copies that image into a host-visible buffer and reads it.

The app's screens are CPU frames, because its filters run on the CPU
([decision 0005](../decisions/0005-video-filters-on-the-cpu.md)). A frame
is 0.77 MB to read back at 1× and 6.9 MB at 3×. In Rust that means the
`ash` crate and MoltenVK (Apache-2.0, so it can ship with the app).
RetroArch's Vulkan driver shows how to do it, but it is GPL. Read it
without copying (PRD 9.4).

The risk is MoltenVK itself. The core already works around some of its
limits, and its strings name three of them.

### 2. A process of its own (medium)

Run the core in a helper process, the way emu-mkxp runs mkxp-z
([decision 0008](../decisions/0008-rpg-maker-games.md)), rather than
loading it into the app:

- **One game per process.** Azahar holds the whole emulated system in a
  process-wide singleton (`Core::System::GetInstance()`). A C++ dylib
  rarely unloads cleanly, so opening a second 3DS game would need a fresh
  process anyway.
- **A crash stays contained.** A crash in the core ends the game, not the
  app and its checkpoint.
- **GPL-3.0 stays separate.** PRD 9.4 and open question Q1 still stand:
  ds-emulator has no licence yet. If the core is downloaded at first use,
  never shipped with the app, and run in a process of its own, the app's
  code stays separate from Azahar's. This is project policy, not legal
  advice.

The helper would be an `emu-libretro` crate and a small binary. It would
speak a link like `link.rs`:

- a hello with the two screen sizes and the rates;
- each frame as two screens;
- a STEP that carries buttons, the circle pad and touch;
- SAVE and LOAD that carry `retro_serialize` bytes.

Unlike an RPG Maker game's states, these hold the whole machine, so they
save and load at any moment, battles included. At 0.2–0.5 s a state,
rewind is still out. Treat the core as not deterministic, as emu-mkxp is:
no rewind, link play or frame-hash tests.

### 3. Getting the core (small)

On the first 3DS game, download Azahar's libretro release for the Mac's
architecture from Azahar's GitHub, at a pinned version. Check the zip
against the SHA-256 that GitHub lists for it; for 2126.2 on arm64 that is
`52c51e8c…7494`. Then unpack it into Application Support. A new version
means bumping the pin. The store checks nothing like this today, so this
check would be new.

### 4. Input (small to medium)

`emu_core::Button` has 12 buttons. The 3DS adds ZL and ZR (New 3DS only),
Home, the circle pad and the C stick, so `Input` needs an analog stick:

- The WASD keys drive the circle pad at full tilt.
- A gamepad's left stick drives it in proportion.
- Touch maps a bottom-screen `TouchPoint` onto the pointer, which covers
  the whole 400×480 picture: x + 40, y + 240, scaled to ±0x7fff.

### 5. Two screens of different widths (small, to check)

The helper cuts the core's picture into a 400×240 top screen and a 320×240
bottom screen. Layouts, touch and screenshots then work per screen. The
window's starting shape is `STACKED_DS`, 256×384
(`crates/emu-app/src/app.rs:87`). The layouts were written for two screens
of the same width, so they need checking at 400 and 320.

### 6. The library and the store (small)

The library should take `.3ds`, `.cci`, `.cxi` and `.3dsx` files and
their `z` forms as 3DS games. It should refuse an encrypted game with a
clear message before starting the helper. Each NCCH partition's header has
a NoCrypto flag (bit 2 of byte 0x18F), so the check reads a flag and
decrypts nothing.

Universal-DB lists 126 games for the 3DS alone:

| Download | Games |
|---|---:|
| A `.3dsx`, which the core opens directly | 69 |
| Only a `.cia`, which it can't open | 22 |
| Only archives (`.zip`, `.7z`, `.rar`), which may hold a `.3dsx` | 25 |
| Nothing | 10 |

The compatibility sweep (`scripts/store-compat.py`) could run them through
the helper with the software renderer, as it does other games through
emu-headless.

## A stopgap: open the game in Azahar's own app

The library could hand a 3DS game to an installed Azahar app, the way the
fan-games research suggests opening Reborn's own Mac app. This is about a
day's work. Citra's app took a game path on its command line; check that
Azahar's still does. The game would play in Azahar's window, with Azahar's
controls and saves. That loses what emu-mkxp was built for: the app's
window, bindings and save states.

## Plan

Update, 2026-11-03: steps 1 to 4 and 6 are done, in the `emu-3ds` crate
([decision 0011](../decisions/0011-3ds-games.md)). The helper is the app's
own executable, as `--3ds-helper`, rather than a binary of its own. The
Vulkan path works: Craftus Reloaded's title ran at about 1,100 to 1,500
fps unpaced, against 28 in software. Home is left out, since the core uses
that button to swap screens. One finding is new: under Vulkan, a state
can't be loaded for `.3dsx` homebrew, which has no title ID, because
Azahar makes the rebuilt renderer's pipeline cache only for a title it
can name. The helper refuses those states; this is worth reporting
upstream.

1. ~~**Prototype Vulkan outside the app.**~~ Done, with `ash` and
   MoltenVK 1.4.2.
2. ~~**Build the helper and its `Core` in the app.**~~ Done.
3. ~~**Add input:** the circle pad, ZL, ZR and touch.~~ Done.
4. ~~**Wire up the library:** 3DS files, the encrypted check and the core
   download.~~ Done.
5. **Add 3DS homebrew to the store,** and run the compatibility sweep. Also
   open `.cia` homebrew by taking out its executable: an unencrypted
   CIA's first content is a plain NCCH that the core opens, and 22 of the
   store's 3DS games ship only a `.cia`.
6. ~~**Write decision record 0011,** and update PRD section 4 and roadmap
   Phase 5 to match.~~ Done.

Testing a commercial game needs the player's own decrypted dump.
