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
