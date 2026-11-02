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

1. **The game runs in its own player, hidden.** `emu-mkxp` copies the
   game's player app into `~/Library/Application Support/ds-emulator/RPG
   Maker/<CRC-32 of the folder's path>/`, marks the copy `LSUIElement`,
   gives it a bundle ID of its own, signs it ad hoc, and starts it with
   `open -n -j -g` and `SDL_MAC_BACKGROUND_APP`: no window shows, no Dock
   icon, no focus taken, and it still draws. Its `mkxp.json` is the
   game's, with the game folder set and a bridge script first among the
   preloads. ds-emulator links no mkxp-z code.
2. **A bridge script paces it.** The bridge replaces `Graphics.update`:
   each frame it sends the screen over a Unix socket and waits for the
   next step's buttons, so ds-emulator's pacing, pause and fast-forward
   are the game's (the bridge lifts mkxp-z's own frame limit). mkxp-z's
   transitions loop natively, unseen and unpaced, so the bridge fades a
   frame at a time instead. `link.rs` describes the protocol.
3. **Sound comes through a FIFO.** OpenAL Soft's WAV writer backend
   writes the game's 48 kHz mix to a named pipe that a thread reads.
4. **States are the game's saves.** The bridge asks the game's preload
   for a state (`DSEmulator.on_save_state`) and hands one back
   (`on_load_state`), then restarts the game, whose preload loads it in
   place of the title. For Essentials that is what Continue loads, made at
   any moment but in a battle; a message showing is put back to its start,
   so it shows again rather than waiting for a box that is gone.
5. **The core contract bends for it.** `Core::try_save_state` may refuse
   with a reason, `saves_every_frame` is false so the app neither rewinds
   nor saves a state unasked, and `ended` says why the game stopped, which
   the app shows.
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

Even so, the sound reached the buffer only after each frame, and a
frame that loads a map can take a third of a second while the music plays
on: replayed, 0.1 s of silence and 0.4 s dropped a minute without Game
Mode. The thread that reads the pipe now plays the sound as it comes
(`audio::Tap`), at normal speed as before, and the buffer starts half
full whenever sound starts again; replayed, nothing was lost. Frames
still get their sound, for recordings. The game's mix peaked at −8 dBFS a
second, −2.3 at the loudest, and never clipped, so OpenAL Soft's limiter
never acted: its loudness is the game's own.
