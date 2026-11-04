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
8. **An installable title plays as the executable it holds.** The core
   cannot install a `.cia`, but an unencrypted one's first content is a
   plain executable (NCCH), which the core opens as a `.cxi`. The app
   finds that content through the CIA's header and title metadata
   (`emu_3ds::cia`, after 3dbrew's CIA and title metadata pages) and,
   on the loading thread, writes it a piece at a time into the games
   folder as `<name>.cxi`, through a temporary file. It is written once
   and used again while it is the same executable: as long, and alike in
   its first MiB, which holds its header and the hashes over the rest. A
   title whose executable is encrypted, with its title key or with its
   NoCrypto flag clear, is refused with a message saying so; nothing is
   decrypted. The session stays the `.cia`'s: its states and checkpoint
   sit beside it and are known by its checksum, as every other game's
   are, and the library titles it from its executable's icon without
   writing anything.
9. **A game's extra data counts as its saves too.** Some games keep
   saves in extdata on the SD card as well as in their save data. Its ID
   is in the executable's extended header, in the storage info, and
   Azahar keeps it in `extdata/<HIGH>/<LOW>/`, in capitals. An ID of zero
   means none: the 3DS lets an application open only the extdata its
   extended header names. With extended save data access, the field lists
   save IDs instead, and the app takes the first, as Azahar does.

## Consequences

- A 3DS game opens from its file (`.3ds`, `.cci`, `.cxi`, `.3dsx`,
  Azahar's compressed forms and `.cia`), the library, drag and drop or an
  archive, and plays in the window with the app's bindings, pause,
  fast-forward, screenshots, recordings, save-state slots and checkpoint.
  The first one waits for a 67 MB download.
- Games must be the player's own decrypted dumps. The core refuses an
  encrypted game and the app holds no keys (PRD 9.5); the app passes on
  the core's words. It refuses an encrypted `.cia` itself, before the
  helper starts.
- A `.cia` takes as much disk again as its executable, which is kept in
  the games folder. If the `.cia` is in the games folder too, the two
  share a name, so its states keep its extension: `Game.cia.ss1`.
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
  or its extdata (`extdata/<id>/`) has changed since. An encrypted game's
  extended header can't be read, so its extdata is not watched.
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

Two free homebrew `.cia`s, Craftus Reloaded's and Button Tester's, each
gave an executable byte for byte the same as a reference extractor's, and
emu-headless played each for 300 frames from its `.cia`. Craftus's title
screen showed, and opening it again used the executable already written.
Neither names extdata.

## Update, 2026-11-04: first-time freezes

Pokémon Omega Ruby froze for up to 2 s at a time whenever it first drew
something new. A game builds a Vulkan pipeline the first time it draws
something new, and its frame waits for it; MoltenVK translates the
pipeline's shaders to Metal, and macOS compiles them. With macOS's shader
cache warm the same freezes took at most 73 ms, so nearly all of the wait
was compiling. A profile of the helper showed why it was so long: Azahar
builds pipelines on the game's thread and on nine workers, all through
one pipeline cache, and MoltenVK holds that cache's lock while it
translates and compiles each shader, so every compile waited for the one
before. More than half of the game thread's wait was for the lock.

1. **One pipeline cache to a thread** (`emu_3ds::pipelines`). The helper
   hands the core its own `vkGetInstanceProcAddr` and
   `vkGetDeviceProcAddr`, which give MoltenVK's functions but for those
   that make, use, read and destroy pipeline caches. Each thread that
   builds pipelines through one of the core's caches builds them through a
   cache of its own, made from the same saved data, so threads compile at
   once. When the core reads its cache to save it, the threads' caches are
   merged into it first, so the next start finds every shader. Building
   with no cache at all was as quick the first time, but threw away
   MoltenVK's translations: the next start took 9 s instead of 0.5 s.
   `DS_EMULATOR_3DS_PIPELINE_CACHE=shared` turns the split off.
2. **Vertices shaded on the CPU** (`citra_use_hw_shader` off), by the
   core's shader JIT. On the GPU every new pipeline also compiles the
   game's vertex shader; on the CPU it is a fixed one, so far fewer
   pipelines compile. It costs about a millisecond a frame more on
   average, and a game that draws heavy scenes every other frame takes
   up to 8 ms more on those frames, which shows as more frames held a
   refresh longer at 120 Hz: 6 a second against 2 to 3 in Omega Ruby's
   opening once everything was compiled. A first playthrough meets new
   scenes all the time, so the shorter freezes were worth more.
   `DS_EMULATOR_3DS_OPTIONS=citra_use_hw_shader=enabled` shades on the GPU
   again; a game much heavier than Omega Ruby might need it.
3. **The helper's threads ask for the window's priority.** The thread
   running the core and the app's thread reading its frames take the
   user-interactive quality of service (see decision 0003's update).

MoltenVK's `MVK_CONFIG_SHOULD_MAXIMIZE_CONCURRENT_COMPILATION` made no
difference while compiles waited for the lock, nor after; turning off
Azahar's accurate multiplication shortened the freezes by a quarter but
changes how games draw, so it stays on.

Measured on the M5 Max with `cargo run --release -p emu-3ds --example
play` (now also giving the spread of frame times and the slowest), 3,600
frames of Omega Ruby's opening with A pressed now and then, each run in an
app bundle of its own so that macOS's shader cache started empty too:

| First run | Frames over 16.7 ms | Time over, in all | Longest frame |
|---|---|---|---|
| Before | 21–23 | 10.0–10.6 s | 1.85–2.09 s |
| One cache to a thread | 25 | 7.0 s | 1.11 s |
| Both | 21 | 2.0 s | 0.26 s |

Played again, with the caches made, every way of running it was alike: 3
frames over 16.7 ms and 34–45 ms over in all, and the game started in about
0.5 s. In the release app, from empty caches, 40 s of the same opening
froze for 7.9 s in all before, the longest 1.96 s, and 1.7 s after, the
longest 0.25 s. States still saved and loaded, which rebuilds the core's
renderer and its pipeline cache.
