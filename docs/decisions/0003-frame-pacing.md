# 0003 — Frame pacing from a timer, audio kept in sync by rate control

- Status: accepted
- Date: 2026-10-05
- Deciders: Luke Fournier
- Settles: PRD open question Q4

## Context

The emulation thread must run frames at the system's rate (59.73 Hz for GB
and GBA, 59.83 Hz for DS) while the display refreshes at 60 or 120 Hz and the
sound card runs on its own clock. Driving emulation from the audio device
gives perfect sound but couples timing to one device. Driving it from
display vsync is not possible from a background thread under GPUI.

## Decision

The emulation thread sleeps until each frame's deadline
(`FrameRate::frame_duration`). Audio goes through a ring buffer to the
device; the resampler adjusts its ratio by up to ±0.5% to keep the buffer
half full (`emu-app/src/resample.rs`). The UI redraws whenever a new frame
is published.

Update, 2026-10-08: macOS wakes a sleeping thread late by up to a quarter
of the time asked for, so one sleep to a deadline about 10 ms away woke
2 to 2.5 ms late. The thread now sleeps three quarters of the time left,
again and again, until it is within 0.1 ms (`speed::sleep_until`), which
takes two or three wake-ups and no spinning. Only resuming from a pause
moves the deadline up to the present; before, every command did, so each
key press pushed the following frames back by the late wake-up, and the
game ran 0.6% slow while a key was being tapped.

## Consequences

- Emulation speed is exact on average and independent of the display and
  sound card. With no audio device, everything still runs.
- The ±0.5% correction absorbs clock drift without audible pitch change.
- Displayed frames can be uneven by one refresh (a 59.73 Hz source on a
  60 Hz display repeats a frame about every 4 seconds). Revisit with a
  frame-blending option or vsync-locked mode if this is visible in play.
- Measured in the release app with Pokémon White on a 120 Hz display,
  10 s at a time: frames woke 2.0 ms late (2.6 ms at the 99th percentile)
  before the stepped sleep, and on time (under 0.3 ms) after. The
  standard deviation of the intervals between published frames fell from
  0.9-1.5 ms to 0.2-1.0 ms, and of those between frames the window drew
  from 2.7-2.9 ms to 1.4-2.3 ms; frames drawn for one refresh or three
  instead of two fell from 22-35 to 6-25 per 10 s. The rate went from
  59.4-59.5 fps to 59.8, the DS's own.
- The view still waits for the display's next refresh to draw a frame:
  4 ms on average at 120 Hz. From a key press to the frame that used it
  reaching the view takes about 16 ms: half a frame waiting for the next
  frame to start, the frame itself, and that refresh. A vsync-locked mode
  could shave the refresh wait and the remaining uneven frames.

Update, 2026-10-26: frames now keep in step with the display
(`refresh.rs`), for ProMotion displays above all. At normal speed and at
fast-forward speeds that allow it, frames last a whole number of
refreshes, or a whole number of frames fit a refresh, when that is
within 0.5% of the frame asked for: two refreshes at 120 Hz for the GB,
GBA and DS, three for a 40 fps RPG Maker game, one frame a refresh for
the DS at 2× or This Gym of Mine at 3×. The window reports when it takes
each frame, and each frame's start is nudged by up to 1 ms so it is
ready 2 ms before the refresh that shows it; a frame that just missed
its refresh counts as late, not early. Sound plays as much faster as the
frames then run, 0.29% for the DS and 0.46% for the GB and GBA, which
the resampler's ±0.5% could not absorb on its own (`audio.rs` tests it).
Fast-forward that fits no whole number, a display none fits (144 Hz),
and a window that draws nothing keep the system's own clock. From a
frame's start to the window drawing it fell from 6.1 to 3.4 ms in This
Gym of Mine and from 6.9-12.6 to 6.2 ms in Pokémon Platinum, measured
with `scripts/macos/bench.sh`. The 2 ms margin lets a frame run up to
that much slower than usual and still catch its refresh; slower ones,
as when an RPG Maker game loads a picture, still show a refresh late.

Update, 2026-11-04: two changes for games whose frames take uneven times.
- **The emulation thread asks for the window's priority.** A thread an app
  starts runs at macOS's default quality of service whatever starts it, a
  class below the window's, so it waited behind other work and its timers
  were let slip: with another app compiling on every core, Pokémon
  Platinum's thread woke up to 40 ms late (12–21 ms at the 99th
  percentile), and its frames took 19–21 ms at the 99th percentile. It now
  takes the user-interactive class (`qos.rs`), and under the same load
  woke at most 1–4 ms late (0.02 ms at the 99th percentile), its frames
  14 ms at the 99th percentile. So do the 3DS helper's thread running
  the core and the app's thread reading its frames.
- **The margin follows the game.** A frame was due 2 ms before its
  refresh, so one that took 2 ms longer than usual missed it. `Lock` now
  keeps the last 120 frames' lengths, from start to publishing, and picks
  the least margin, in steps of a quarter millisecond, that keeps about
  the most of them inside one refresh, with 1 ms to spare at each end and
  up to one frame in 50 given up: more margin lets slow frames catch their
  refresh, but too much shows quick ones a refresh early. Frames that take
  about as long as each other keep 2 ms. Each frame's own length is taken
  out of the error that moves the next start, as its start, not its length,
  is what the lock controls. In Pokémon Platinum, with nothing else busy,
  the margin settled at 2.25–2.75 ms and frames held a refresh too long or
  short went from 0.8–2 to 0.5–0.8 a second; in Omega Ruby's opening, from
  3.4–4.1 to 2.2–2.8 a second with vertices shaded on the GPU. The trace
  logs the margin as `margin <µs>`.

Update, 2026-11-23: fast-forward shows only the frames the display has
refreshes for, and the rest are run without a picture.
- **Which frames.** While the display has a refresh for every frame, all
  are shown, as before: the DS at 2× on a 120 Hz display, or at 3× on a
  240 Hz one. Past that, one frame in N is shown, N being the fewest
  frames that together last a whole number of refreshes
  (`Lock::shown_every`): one DS frame in three at 3× and 120 Hz, each for
  two refreshes, and one in two at 4×. Showing two in three, as many as
  the display could take, would hold them for one refresh and two in
  turn. N stops where the frames shown would come further apart than the
  system's own; a display nothing fits (144 Hz at 3×) gets as many as it
  has refreshes for. Flat out, one frame is shown each refresh. The
  frames shown are what `Lock` keeps in step with the refreshes, so 3×
  is now locked too, where it ran on the system's clock before.
- **What the rest skip.** The emulation thread tells the core before each
  frame (`Core::set_frames_shown`) and publishes only the frames shown.
  The DS core then draws neither its larger 3D frame nor its larger
  screens (PRD-upscaler UP-12); the native frame, which display capture
  reads and states save, is drawn all the same. A recording, a script
  and the debugger get every frame.
- **Measured** on Pokémon HeartGold, indoors with 3D on screen, from a
  checkpoint, 3D at 2×, one frame in three shown (`emu-headless
  --show-every 3`): the core retired 15% fewer instructions (162 to
  138 billion in 1,500 frames; 24% fewer at 4×), with the same sound,
  the same state and the same shown frames to the bit. It is not two
  thirds of the larger 3D's cost, as the game draws a new 3D scene only
  every second frame, and one scene in three is now skipped where none
  was. In the app at 3× the trace showed 180 frames run and 60 shown a
  second, 16.7 ms apart. Its CPU with the window in view was not
  measured: the bench ran behind other windows, which draw nothing.
