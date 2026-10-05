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

## Consequences

- Emulation speed is exact on average and independent of the display and
  sound card. With no audio device, everything still runs.
- The ±0.5% correction absorbs clock drift without audible pitch change.
- Displayed frames can be uneven by one refresh (a 59.73 Hz source on a
  60 Hz display repeats a frame about every 4 seconds). Revisit with a
  frame-blending option or vsync-locked mode if this is visible in play.
