# 0008 — RPG Maker games: mkxp-z in a hidden process, a step at a time

- Status: proposed
- Date: 2026-10-24
- Deciders: Claude, for Luke Fournier's review

## Context

Pokémon fan games such as Pokémon This Gym of Mine are made with Pokémon
Essentials on RPG Maker XP: Ruby scripts on Enterbrain's Windows runtime.
They are no ROM, and there is no console to emulate. Wine does not run them
on Apple Silicon, but mkxp-z, an open-source RPG Maker player, runs their
scripts natively on Ruby 3.1 once a preload script adapts the Windows-only
parts. Such a game ships as a folder: `Game.ini`, `Data/`, `Graphics/` and,
for the Mac, a mkxp-z player app with its `mkxp.json`.

The player wanted these games in ds-emulator's own window, with its
controls, pause, fast-forward, screenshots, recordings and save states, not
in a window of their own.

mkxp-z is GPL-2 and a program, not a library: it owns the main thread and
an SDL window and event loop, as GPUI does.

## Decision

1. **The game runs in its own player, hidden.** On macOS, `emu-mkxp`
   copies the game's player app into `~/Library/Application
   Support/ds-emulator/RPG Maker/<CRC-32 of the folder's path>/`, marks the
   copy `LSUIElement`, gives it a bundle ID of its own, signs it ad hoc,
   and starts it with `open -n -j -g` and `SDL_MAC_BACKGROUND_APP`: no
   window shows, no Dock icon, no focus taken, and it still draws. Its
   `mkxp.json` is the game's, with the game folder set and a bridge script
   first among the preloads. ds-emulator links no mkxp-z code. Linux and
   Windows run mkxp-z's own program instead; see the update of 2026-11-06.
2. **A bridge script paces it.** The bridge replaces `Graphics.update`:
   each frame it sends the screen over a Unix socket (TCP on Windows) and
   waits for the next step's buttons, so ds-emulator's pacing, pause and
   fast-forward are the game's (the bridge lifts mkxp-z's own frame
   limit). mkxp-z's transitions loop natively, unseen and unpaced, so the
   bridge fades a frame at a time instead. `link.rs` describes the
   protocol.
3. **Sound comes through a FIFO.** OpenAL Soft's WAV writer backend
   writes the game's 48 kHz mix to a named pipe that a thread reads: a
   FIFO on macOS and Linux, a `\\.\pipe\` pipe on Windows.
4. **States are the game's saves.** The bridge asks the game's preload
   for a state (`DSEmulator.on_save_state`) and hands one back
   (`on_load_state`), then restarts the game, whose preload loads it in
   place of the title. For Essentials that is what Continue loads, made at
   any moment but in a battle; a message showing is put back to its start,
   so it shows again rather than waiting for a box that is gone.
5. **The core contract bends for it.** `Core::try_save_state` may refuse
   with a reason, `saves_every_frame` is false so the app neither rewinds
   nor saves a state unasked (until 2026-11-02: see the last update), and
   `ended` says why the game stopped, which the app shows.
6. **Each game's preload maps the buttons.** The bridge reports the DS
   buttons held (`DSEmulator.pressed?`); the game's compatibility script
   decides what each does, as Essentials replaces RGSS's input with its
   own.

## Consequences

- An RPG Maker game opens from its folder, from the library or by drag
  and drop, and plays in the window with the app's bindings, gamepad,
  pause, fast-forward, screenshots, recordings and key guide.
- Not deterministic: no rewind, no link play, no cheats, no debugger or
  memory viewer, and Lua scripts can press buttons and save states but
  read no memory.
- Loading a state restarts the game, so the screen goes black for a
  moment. A state saved while
  an event's script runs, rather than its message, runs that script again
  when loaded, so an item given just before could be given twice.
- A game plays only with a preload that reads `DSEmulator`'s buttons and
  makes states; one without loads but takes no input. Each game needs its
  own compatibility script, as This Gym of Mine's `tgom_compat.rb`.
- The game's own in-game save is the same file whether it plays in the
  app or on its own.

## Verification

With a copy of Pokémon This Gym of Mine 4.2.2, a test harness drove
`emu-mkxp` through a new game's intro by pressing A. States of about
93 KB saved in about 1.5 ms. A state saved on the map, then a walk,
then a load put the player back where they were; one saved mid-dialogue
showed that line again on loading and the scene ran to its end. X opened
the menu, and Save there wrote the game's save, which Continue loaded
after a reset. In the release app, a Lua script saved and loaded slots 1
and 2 in turn without a hang, and the hidden player quit with the app.
The game runs at about 116 fps unpaced, 2.9× its 40.

Update, 2026-10-26: a frame's round trip to the player took 10.3 ms at
40 fps, nearly all of it the bridge reading the screen back from the
GPU: 7.8 ms under mkxp-z's Metal renderer. The hidden player now draws
with OpenGL unless the game chose a renderer (1.2 ms, the same pixels:
frames 120-124, 300, 600, 601 and 1300 hash the same), is not
Retina-capable (under OpenGL on a Retina screen the bridge read back a
quarter of the screen), and skips frames, so it never draws in its unseen
window, which stalled a frame by 10 ms every 50 or so. The bridge's
socket has room for a whole frame, where the usual 8 KB sent it in a
hundred pieces, and each frame's pixels are freed as soon as they are
sent, which stopped Ruby's minor collections. Measured with
`scripts/macos/bench.sh` on This Gym of Mine's title screen at 40 fps,
the round trip fell to 1.4 ms, a key reached the window 3.4 ms after its
frame started rather than 17.5, and the player's CPU fell from 41% to
6%. At 3× fast-forward the game now holds 120 fps.

Update, 2026-10-31: frames dropped in This Gym of Mine's later areas.
Under the OpenGL renderer the hidden player uses, each of mkxp-z's draw
calls has a high fixed cost: about 59 µs a `blt` and 230 µs a
`draw_text`, whatever its length (20 and 31 µs under Metal, where reading
a frame back took 3.6 ms rather than 0.27, so OpenGL stays). Essentials
v18 draws with many small calls. Its `CustomTilemap` drew the map's
ground into a bitmap a tile at a time, and every 10 frames, as water and
flowers animate, drew each animated tile again: 27 ms on maps with much
water (Breezee Dam, Windswept Farms, New Day Plain), longer than a whole
40 fps frame, and 30 to 190 ms on entering a map or every 5 tiles walked.
Messages took 4 to 9 `draw_text` calls a character, and windows and plain
color planes a `blt` per tile of their skin: 30 to 48 ms as a battle
animation or a window starts.

The game's compatibility script (`tgom_compat.rb`, in the game's folder,
not this repository) now has mkxp-z's own `Tilemap` draw the ground in a
viewport of its own, just beneath the map's, which tints it after the
tiles are laid as `CustomTilemap` did; draws each piece of text with its
shadow once and keeps it (at most 1,024 pieces, 16 MB); fills color
planes in one call; and tiles window skins by doubling. A map with still
water, whose reflections show through the ground, keeps the old ground.
Measured with a probe preload in a copy of the game, which starts a game
unseen, walks every map and fights wild battles, frames over 8.3 ms (the
budget at 3×) fell from 327 to 75 of 24,431 on the maps, and from 2.6% to
0.13% of battle frames; the 99th percentile fell from 14.6 to 1.8 ms, and
the player's work from 1.25 to 0.97 ms a frame. What stays slow is
entering a map, behind its fade: Essentials' own `GC.start` (12 ms) and
loading pictures. Screenshots of every map at noon, evening and midnight,
and where maps meet, match pixel for pixel, but for a reflection the old
ground let show where the probe stood the player over a cave's empty
space; text is within 2/255, also under Metal. A save state saved and
loaded, and 75,000 frames held memory flat.

Update, 2026-11-02: at normal speed This Gym of Mine showed 40 fps, where
3× fast-forward showed 120, and its sound broke up. 40 fps is RPG Maker
XP's own rate, each frame three refreshes of a 120 Hz display; the sound
was the fault. While ds-emulator plays full screen, macOS's Game Mode
makes it the focal game and throttles the hidden player, a game of its
own to macOS (its `Info.plist` says so), as one left in the background:
role 6, "Darwin background", priority 4, efficiency cores only, its timers
held back. OpenAL Soft's WAV writer sleeps 5 ms between writes; held back,
it woke every 50 to 100 ms and wrote all it owed at once, so the sound came
in bursts of up to a tenth of a second, against a buffer that holds a
tenth and keeps half. Replayed against that buffer, a 90 s walk through
the maps had 8.7 s of silence and dropped 9.2 s of sound.
`LSAppNapIsDisabled`, `NSAppSleepDisabled` and a user-initiated,
latency-critical `NSProcessInfo` activity in the player did not stop it.
A process may set the role of another of the same user, though, and one
set back stays so. The bridge now takes ds-emulator's role, that of the
process at the link's other end, every 8 frames, through Ruby's Fiddle;
a demotion is undone within 0.2 s. With its role back under Game Mode,
the player ran at priority 54 and its sound came every 21 ms, as stdio
fills its 4 KB buffer.

Update, 2026-11-08: the app now opts out of Game Mode
(`LSSupportsGameMode` false in its `Info.plist`), as Game Mode also slows
every other app, and video playing beside a game stuttered. With the key,
gamepolicyd no longer counts a full-screen games app as a game, and Game
Mode stays off; without it, the same test app turned it on. The bridge
still copies the app's role, which now keeps the player at the app's own.

Even so, the sound reached the buffer only after each frame, and a
frame that loads a map can take a third of a second while the music plays
on: replayed, 0.1 s of silence and 0.4 s dropped a minute without Game
Mode. The thread that reads the pipe now plays the sound as it comes
(`audio::Tap`), at normal speed as before, and the buffer starts half
full whenever sound starts again; replayed, nothing was lost. Frames
still get their sound, for recordings. The game's mix peaked at −8 dBFS a
second, −2.3 at the loudest, and never clipped, so OpenAL Soft's limiter
never acted: its loudness is the game's own.

Later that day: screens that "stopped taking buttons", as choosing a move
to forget for a new one, had crashed. Ruby 3 rejects a `sprintf("%d%")`
that Ruby 1.8 printed as it was, and Essentials' `using(window)` is
shadowed by Ruby's own `Module#using` where `self` is a module. The game's
compatibility script now fixes both. An error that escapes a game's
scripts used to show in mkxp-z's message box, unseen in the hidden player,
while ds-emulator held the last frame for 30 seconds and then said the
game had stopped responding. The bridge now wraps the game's main script:
an error writes its backtrace to the player's error log and quits the
player at once, so ds-emulator says within a moment that the game stopped
and why. Resets and the game's own exits pass through.

Update, 2026-11-02, later: This Gym of Mine lost the player's place.
Every other game keeps a checkpoint, a state in slot 0 saved as the player
closes it, quits, opens another game or opens the game menu, and picked up
when it next starts. An RPG Maker game skipped it, as one that "saves
itself", so all it kept was what the player saved in the game's own menu.
The player had never saved there. A crash in the game's scripts that day
had already lost a session, and closing the app lost the next.

Its checkpoint is now saved as every other game's is. The game cannot be
saved everywhere, as in a battle, and its player can stop on an error, so
the checkpoint is also saved every minute it plays, five seconds later
again if the game could not be saved then. One costs about 1.5 ms in the
player and 4 ms to write 0.7 MB, once a minute. A refusal keeps the
checkpoint before. A checkpoint must never be picked up over a save the
game made itself since, in the app before a crash or played on its own:
its record lists the game's own saves, the files with RPG Maker's save
extensions (`rxdata`, `rvdata`, `rvdata2`) directly in the game's folder
and in the folder mkxp-z keeps its files in, which the bridge now says in
its hello (`System.data_directory`; the link is version 3). If one has
changed, the game starts at its title, where Continue loads it. A
checkpoint with no record is left alone, as it cannot be undone as a
cartridge's can.

Checked through the emulation thread with a copy of the game that saves to
a folder of its own: a new game's intro, played to the first map and
closed mid-message, opened again at the same place and line rather than
the title; the checkpoint was saved again during a minute's play; and with
the game's own save changed, it started at the title.

Update, 2026-11-06: Linux and Windows. The game plays the same way, in a
hidden mkxp-z driven by the same bridge, but the player is mkxp-z's own
program rather than a Mac app.

**Hiding the player.** Tried in an Ubuntu 24.04 container with mkxp-z's
own Linux build (CI run 37110075912, `37a04d1`) and a small RPG Maker XP
game made for the test:

| Way | Result |
|---|---|
| `SDL_VIDEODRIVER=offscreen` | Not built in: the builds' SDL 2.28.1 has X11, Wayland, KMSDRM and dummy only |
| `SDL_VIDEODRIVER=dummy` | No OpenGL, so no window: mkxp-z quits |
| A private Xvfb | Works. Mesa draws on the CPU; 600 tile `blt`s and 30 `draw_text`s a frame took 29.9 ms on one emulated core, about what the per-call costs measured on the Mac's OpenGL add up to, as each call's fixed cost dominates |
| The user's X display, the bridge hiding the window | Works; the frames, drawn into framebuffers, still come. The window shows from mkxp-z's start until the bridge runs: 0.3 to 0.7 s |
| Wayland | The build crashed under a headless Weston before making a window, so it was not tried further |

**Linux.**
- The player is a program named `mkxp-z…` in the game's folder that is an
  ELF program for this processor (as `mkxp-z.x86_64` or `mkxp-z.aarch64`,
  the names its builds have), or else one in the players' folder,
  `~/.local/share/ds-emulator/RPG Maker/mkxp-z/`, for every game. A game
  folder is a `Game.ini` with such a program or a `Game.exe`, whose icon
  the library shows. With neither, the game says where to put mkxp-z.
- It is started directly, one per game as before (`player.lock`), with
  its `mkxp.json` in the game's player folder, which mkxp-z reads from
  `SRCDIR`. The link is a Unix socket and the sound a FIFO, as on macOS.
- It draws on a private Xvfb when Xvfb is installed: Xvfb picks a free
  display (`-displayfd`) and takes only clients with the MIT cookie written
  for it (`-auth`, `XAUTHORITY`), so nothing shows and no other program can
  watch or type into the game. Otherwise it draws on the user's X display
  (XWayland on a Wayland desktop) with `SDL_VIDEODRIVER=x11`, and the
  bridge withdraws the window through Xlib, from the window SDL reports
  (`SDL_GetWindowWMInfo`). SDL's own `SDL_HideWindow` waits for an
  `UnmapNotify` that the player's main thread may read first. With neither
  Xvfb nor an X display, the game says so.
- It draws with the system's OpenGL (renderer 2), which needs Mesa's
  `libGL`/`libEGL`, as every desktop has.
- The builds' Fiddle has its C half but cannot load its Ruby half (`$:`
  is only the game's folder), so the bridge stands in for the error
  setters the C half calls.
- Stopping is `Child::kill` after the grace period; a player that exits
  before its bridge connects ends the wait at once, with its last error,
  rather than after a minute.

**Windows** (not turned on: `UNSUPPORTED` still refuses it).
- The player is `mkxp-z….exe`, or a `Game.exe` that names itself mkxp-z
  inside, as newer Pokémon Essentials games ship, or else one in
  `%LOCALAPPDATA%\ds-emulator\RPG Maker\mkxp-z\`. mkxp-z reads
  `mkxp.json` only from beside its program on Windows, so the program and
  the DLLs beside it are hard-linked, or copied, into the game's player
  folder with the settings.
- Ruby 3.1 has no Unix sockets on Windows, so the link is TCP on
  127.0.0.1, at a port the system picks. Any program can reach that, so
  the player gets a token of 128 random bits (`DS_EMULATOR_TOKEN`) that the
  bridge sends first; a connection that sends anything else is dropped.
  Both ends turn off Nagle's delay, as each step is four bytes.
- Sound comes through a named pipe, `\\.\pipe\ds-emulator-<id>-<pid>`,
  for one writer from this computer. OpenAL Soft opens it as a file and
  takes it for a pipe.
- mkxp-z sends Ruby's errors nowhere on Windows unless it opens a console,
  so the bridge reopens them into the player's error log.
- `SDL_WINDOW_NO_ACTIVATION_WHEN_SHOWN` keeps the keyboard with the app,
  and the bridge hides the player's windows, those of SDL's class that are
  its process's, with `ShowWindow`. The window shows until then; starting
  it hidden takes `STARTUPINFO`, which std's `Command` sets only on
  nightly.
- mkxp-z keeps its own renderer, Direct3D 11 through ANGLE. Its OpenGL
  setting is ANGLE over the driver's OpenGL there, as mkxp-z sets
  `ANGLE_DEFAULT_PLATFORM` where its own `getenv` does not see it.
- Tried under Wine (Ubuntu 24.04, x86-64, through Rosetta): the player
  started from its copy, the link with the token, the sound through the
  pipe, buttons and save states (by the state's CRC), the error log and
  the hidden window all worked, with ANGLE's null renderer, which draws
  nothing. Both of ANGLE's real renderers stopped on Wine's software
  OpenGL (an ES 3 sample-count assertion). No frame has been drawn on
  Windows, so it stays off until a run on Windows draws one: then remove
  Windows from `UNSUPPORTED` and run the test below there.

**Getting mkxp-z.** mkxp-z publishes no releases, only CI artifacts that
expire after 90 days and need a GitHub login, so the app cannot download
it from upstream as it does Azahar. Today a game plays when its folder
has an mkxp-z for the system, or when the player has put one build in
the players' folder, which the error names. Few fan games ship a Linux
build. The way to make it one click is to host pinned builds ourselves,
per system and processor, and download one at first use with its
SHA-256, as for the 3DS core. The upstream builds are GPL-3.0-or-later,
not GPL-2.0 (their `LICENSE.txt`: HTTPS through OpenSSL and ANGLE's
Vulkan backend need version 3), so hosting them means offering the
complete corresponding source beside them, which upstream's
`source-with-dependencies` artifact is (277 MB). That is a decision for
Luke Fournier.

Unchanged everywhere: a game plays only with a preload that reads
`DSEmulator`'s buttons and makes its states.

Verified with a game of the test's own (`crates/emu-mkxp/tests/mkxp_z.rs`,
run with `DS_EMULATOR_MKXP_Z` naming a folder holding an mkxp-z build): on
Linux arm64 and x86-64 (Rosetta) with upstream's builds and Xvfb, the
square starts at (100, 100), Right for 10 frames puts it at (140, 100),
a state saved there and loaded after Down for 5 frames puts it back, and
sound comes. emu-headless with Lua scripts gave the same picture
(`8c3ef513`) for Right then a state round trip as for Right alone, against
`0e013883` idle, and wrote the sound to a WAV, both on a private Xvfb and
on an X display with the window withdrawn.

Update, 2026-11-06, later: mkxp-z downloaded at first use. Luke Fournier
chose to host pinned mkxp-z builds, so an RPG Maker game plays in one
click on Linux and Windows. They are in the public repository
`lilfourn/imago-mkxp-z`, one release per upstream commit. The first,
`mkxp-z-37a04d1`, holds upstream's CI artifacts from run 37110075912
(commit `37a04d14413454551a7b6aadbb4e93e67168accb`, 2026-10-03):
`mkxp-z.linux.ubuntu.xenial.x86_64`, `mkxp-z.linux.ubuntu.xenial.arm64`
and `mkxp-z.windows.cross.x86_64`, and beside them the complete source,
`mkxp-z.source-with-dependencies.dev-37a04d1.tar.zst`.

Why this meets GPL-3.0 §6. The builds are GPL-3.0-or-later, and handing
them out is conveying object code, so §6 applies. We take §6(d):
"offering access from a designated place … and offer equivalent access to
the Corresponding Source in the same way through the same place at no
further charge":
- **Same place, same way, no charge.** The binaries and the source are
  assets of the same public GitHub release, downloaded the same way, free,
  and the repository's README points to both. The app downloads only the
  binary, as §6(d) allows ("you need not require recipients to copy the
  Corresponding Source along with the object code").
- **It is the Corresponding Source** (§1). The source tarball is the one
  upstream's own CI made from the same commit as the binaries. It holds
  mkxp-z's source, its Meson build files, the CI workflow that built these
  binaries (`.github/workflows/autobuild.yml`), the cross files, and
  the source archives and patches of all 56 libraries the build vendors
  (SDL 2.28.1, OpenAL Soft 1.24.3, Ruby 3.1.3, ANGLE, OpenSSL 3.0.8,
  Mesa's headers and the rest). The builds use only those vendored copies:
  upstream's `default_system` option is `disabled`. What §1 leaves out
  is not in it: System Libraries (glibc and `libgcc_s` on Linux, Windows'
  own DLLs) and general-purpose tools (Clang; Ubuntu's MinGW-w64 GCC 13).
  The GCC runtime linked into the binaries (libstdc++, libgcc) comes under
  GCC's Runtime Library Exception 3.1. The Windows build's MinGW-w64 runtime
  (public domain and ZPL) and winpthreads (MIT) are permissively licensed.
  Their notices are in the repository.
- **Unmodified.** Each file's SHA-256 is the digest GitHub recorded when
  upstream's CI uploaded it (`gh api
  repos/mkxp-z/mkxp-z/actions/artifacts/<id>` gives `digest`), so there
  are no modifications to mark (§5a) or to give source for. The
  `LICENSE.txt` in each build is upstream's own.
- **Notices** (§4). The repository has the GPL-3.0 text, upstream's
  `COPYING` and `LICENSE.mkxp-z-with-https.txt`, and the GCC and MinGW-w64
  runtime licences.
- **For as long as needed.** A release's source stays beside its binaries
  for as long as the binaries are offered. A later pin is a new release
  with its own source, and releases are never trimmed to the binaries.
  Upstream's artifacts expire on 2027-01-01; these copies do not.
- **Installation Information** (§6, last paragraphs) concerns User
  Products, devices that run the software. A program downloaded to a PC
  is not one.

How the app gets it (`emu-mkxp`'s `install.rs`). A game plays in the
mkxp-z its folder has for the system, then in one the player put in the
players' folder (`<data>/ds-emulator/RPG Maker/mkxp-z/`), and only then
in the hosted build. That one is downloaded the first time it is needed,
with the app's usual notice ("Downloading mkxp-z, the RPG Maker player,
about 20 MB, the first time only…"). The pins are a URL, size and
SHA-256 per system and processor: Linux x86-64, Linux arm64 and Windows
x64 (which Windows on ARM also runs). curl from `PATH` downloads the zip,
with no console window on Windows, into a temporary folder. The download
is checked against its pin; the program and its `LICENSE.txt` are taken
out, and the folder is renamed into place as `mkxp-z/mkxp-z-37a04d1/`, so a
later pin installs beside it. A lock keeps two installs from racing. With
no build for the system, or a failed download, the game says where a
build can be put instead.

Imago itself stays closed source. It runs mkxp-z as a separate program
that it starts and talks to over a socket and a pipe, as two programs
communicate, and it neither links nor modifies mkxp-z. The bridge script
is a game script that mkxp-z interprets, as it interprets any game's.
Nothing from this repository is in `lilfourn/imago-mkxp-z`.

## Update, 2026-11-10: one installer

The player's download is `emu_os::install` over `emu_os::fetch`, shared
with the 3DS core's and the randomizer's (see decision 0011's update of
the same day).
