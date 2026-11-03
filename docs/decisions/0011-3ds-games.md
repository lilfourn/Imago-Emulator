# 0011 — 3DS games: Azahar's libretro core in a helper process

- Status: accepted
- Date: 2026-11-03
- Deciders: Claude, for Luke Fournier's review

## Context

The player wanted to play 3DS games in the app, in its window and with its
controls, as the GB, GBA, DS and RPG Maker games play. The PRD rules out
writing a 3DS core (section 4): a 3DS emulator is a whole operating system
in high-level emulation, a PICA200 shader translator and a JIT. It allows
hosting an existing one, Phase 5 of the roadmap.
[The research](../research/3ds.md) chose Azahar, Citra's successor
(GPL-3.0), whose every release ships a libretro core for macOS. Its macOS
core draws with Vulkan, which a frontend must provide, or in software;
it has no OpenGL renderer, as macOS stops at OpenGL 4.1. In software it
ran a simple 3D homebrew game at 57 fps of 60 and Craftus Reloaded's
title at 28.

## Decision

1. **Azahar's core, downloaded at first use.** The app downloads the
   pinned release, Azahar 2126.2's macOS libretro core (7 MB), and
   MoltenVK 1.4.2 (Apache-2.0, 60 MB), checks each against a pinned
   SHA-256 and installs them in `~/Library/Application Support/ds-emulator/3DS`
   (`emu_3ds::install`). Neither ships with the app, and the repository
   holds no Azahar code: the project's own code stays apart from
   GPL-3.0 code (PRD 9.4, open question Q1).
2. **The core runs in a helper process: the app's own executable,
   started again with `--3ds-helper`.** `main` hands over to
   `emu_3ds::run_helper_if_asked` before anything else. A process of its
   own suits the core: Azahar keeps the whole console in a process-wide
   singleton, a C++ dylib does not unload cleanly, so each game needs a
   fresh process anyway, and a crash in the core ends the game, not the
   app. The two talk over the helper's stdin and stdout (`link.rs`),
   request and answer, one frame at a time: the app sends what the player
   holds and gets both screens and the frame's sound back. Its stdout is
   kept for the link alone; anything else in the process that prints goes
   to `3DS/helper.log`.
3. **Vulkan through MoltenVK.** The helper is the frontend in libretro's
   Vulkan contract: it creates the instance, the core makes its device
   through its negotiation interface, and the core submits its own work
   under the queue's lock. After each frame the helper copies the image
   the core names into memory the CPU reads and waits for the copy, so
   nothing is in flight between frames. The core records its commands on
   a worker thread: it hands that thread the frame's last commands and
   names the image at once, and the thread submits them 50 to 80 µs
   later. The copy waits for that submission (counted through the queue
   lock), or it would read the frame before, a frame late. A Mac without
   a usable Vulkan device falls back to the software renderer.
4. **Two screens, as the DS's.** The core draws both screens into one
   picture, top over bottom; the helper cuts it into the 400×240 top
   screen and the 320×240 touch screen, so the app's layouts, touch,
   screenshots and recordings work as they do for the DS. View's 3D
   resolution sets Azahar's internal resolution, and the screens come
   out that many times larger, up to 4×: every frame is read back and
   sent to the app, 11 MB a frame at 4× and 44 MB at 8×, which would
   not keep full speed.
5. **The same controls.** The bindings are the app's, for every system:
   the D-pad's (WASD, the arrows, a gamepad's left stick) press the
   D-pad and tilt the circle pad all the way, so a game that moves with
   either moves; U/I/J/K, Q/E, Space and Backspace are the face buttons,
   shoulders, Start and Select; ZL and ZR are new buttons, on Z and C.
   The bottom screen is touched with the mouse, as the DS's is.
6. **States are the core's own, made anywhere, but not every frame.** The
   core serializes the whole console, compressed, in a few hundred
   milliseconds. Like the RPG Maker core it is not deterministic, so
   there is no rewind; the checkpoint is saved when the game closes, but
   not every minute while it plays (`Core::checkpoints_while_playing`):
   it would hitch play, and 3DS games save themselves.
7. **The helper keeps the app's scheduling role.** Every eight frames it
   takes the app's `PRIO_DARWIN_ROLE`, so Game Mode cannot run it as a
   background process, as it did RPG Maker's player (decision 0008).

## Consequences

- A 3DS game opens from its file (`.3ds`, `.cci`, `.cxi`, `.3dsx` and
  Azahar's compressed forms), the library, drag and drop or an archive,
  and plays in the window with the app's bindings, pause, fast-forward,
  screenshots, recordings, save-state slots and checkpoint. The first one
  waits for a 67 MB download.
- Games must be the player's own decrypted dumps. The core refuses an
  encrypted game and the app holds no keys (PRD 9.5); the app passes on
  the core's words. It cannot open an installable title (`.cia`).
- `.3dsx` homebrew has no save states under Vulkan. Loading a state
  rebuilds the core's renderer, and Azahar 2126.2 makes the new
  renderer's Vulkan pipeline cache only for a game with a title ID;
  without one, the next frame crashes. The helper refuses states for such
  games rather than make ones it can't load. Cartridges, executables and
  installed titles have title IDs, and their states load.
- No rewind, link play, cheats, debugger or memory viewer, and Lua
  scripts can press buttons and save states but read no memory.
- A game's own saves are the core's: they live in its NAND and SD card in
  `3DS/Azahar`, whatever the app does with states. A checkpoint is not
  picked up once the game's save data (`title/<id>/data/` on the SD card)
  has changed since; saves a game keeps in extra data (extdata) are not
  watched.
- The helper's log is `3DS/helper.log`, and the start before's is kept as
  `helper.previous.log`. Once the helper stops, the game ends with why,
  and nothing more is asked of it.
- Homebrew that plays sound through the DSP needs the DSP firmware a
  console dumps (`sdmc:/3ds/dspfirm.cdc`); commercial games carry their
  own.
- A new Azahar release means bumping the pinned version and digests.

## Verification

On an M5 Max with macOS 27.0.1, `cargo run --release -p emu-3ds --example
play` drove the core through the helper, as the app does, with frames
unpaced. Craftus Reloaded's 3D title screen ran at about 1,100 to 1,500
fps with Vulkan, against 28 in software; at 2× internal resolution, about
720; at 4×, about 230 to 270, each frame read back and sent. Both screens
matched the software renderer's split. A state of Craftus Reloaded, as the
executable its `.cia` holds, saved in about 200 ms (0.8 MB) and loaded in
about 330 ms, and play went on; the same game as `.3dsx` crashed on the
first frame after a load, which is why such states are refused.

In the release app, with a home folder of its own: Craftus Reloaded opened
from its `.cxi`, the helper started beside it, and it played in the window
at 59 to 61 fps, both screens stacked, with the key guide showing ZL and ZR.
Quitting saved its checkpoint (1 MB, with its record) and the helper
exited; opening it again picked the checkpoint up. emu-headless, starting
with nothing installed, downloaded and installed the core and MoltenVK and
played 900 frames in 15 s, most of it the download. Holding Right from a Lua
script moved 3d-platformer's player and scrolled its level.
