# 0013 — Upscaling runs on the GPU, through Metal, inside the app

- Status: accepted
- Date: 2026-11-04
- Deciders: Claude, for Luke Fournier's review
- Revisits: [decision 0005](0005-video-filters-on-the-cpu.md), point 1

## Context

The owner asked for an upscaler that draws every game at the resolution of
the screen it is on, on every kind of display
([PRD-upscaler.md](../PRD-upscaler.md),
[research](../research/upscaling.md)). The displays used need 2.3 to 18
device pixels per console pixel, rarely a whole number.

Decision 0005 put video filters on the CPU, to be revisited "if a wanted
filter cannot run in budget on the CPU". The pixel-art upscalers that draw
at any factor (OmniScale, xBR) work per output pixel: about 71 ns a pixel
on a CPU (the MMPX paper's figure for xBR), or about half a second for a
Game Boy frame filling the 240 Hz external display. The fixed-factor CPU
scalers (Scale2x, Scale3x, MMPX) cannot fit a display without GPUI's
linear filter blurring what they draw. So the condition is met.

GPUI 0.2.2 still has no shader hook, samples every image linearly, and
its only zero-copy surface takes 4:2:0 YCbCr, which smears pixel art.

## Decision

1. **Upscalers run on the GPU through Metal, in `emu-app`**
   (`gpu_upscale.rs`), and their output becomes an ordinary GPUI image: up
   to a texture, enlarged, copied to a shared buffer, read back. A frame's
   round trip measured 0.15–0.9 ms up to about 5 million output pixels on
   an M5 Max, and the app draws each new frame's screens in 1.1–1.4 ms at
   1.5 million pixels.
2. **Each screen is drawn at its placement's exact size in device
   pixels**, recomputed whenever the window, display or layout changes, and
   placements snap to whole device pixels so the image is drawn one to
   one. A budget of 6 million pixels a frame keeps the cost bounded on
   large displays; GPUI's linear filter enlarges past it.
3. **Three ways to draw, all licensed for reuse:** Sharp Pixels, the
   default for pixel art, the project's own kernel that keeps every console
   pixel a crisp, even square at any size; Apple's MetalFX spatial scaler
   for smooth pictures; and OmniScale, ported from SameBoy's shader
   (Expat, notice kept), which reshapes pixel art and is opt-in only, as
   Rounded Pixels, after the owner rejected it as the default (PRD 6.2).
   Shaders compile from source at run time, as GPUI's do, so the build
   needs no Metal toolchain.
4. **An `unsafe` exception for one module.** objc2's Metal and MetalFX
   calls are `unsafe`. `gpu_upscale` is marked `#[allow(unsafe_code)]`,
   every block has a `// SAFETY:` comment, and the CPU only touches a
   texture or buffer once the GPU's work on it is finished: one batch is
   on the GPU at a time, and the next is uploaded only once it is taken.
   Since November 2026 a draw waits for a batch at most 4 ms, as with
   other apps busy on the GPU one in a hundred took 14–30 ms; a slower
   one is put in place by a later draw.
5. **The CPU path stays** for Sharp Pixels, the LCD grid and scanlines,
   while the GPU starts, and on a Mac without Metal, where Scale2x and
   Scale3x stand in for Rounded Pixels.

Not chosen: a `CAMetalLayer` of the app's own, which would cover GPUI's
menus, notices and script overlays; and patching GPUI for BGRA surfaces,
kept for later if the copies cost too much (PRD-upscaler UP-16).

## Consequences

- Every display gets a picture made for it, with no fractional blur from
  the sharp-bilinear pre-scale, and the same frame looks the same at any
  window size.
- Each frame makes a CPU → GPU → CPU round trip and an upload into GPUI's
  atlas. That is the cost to watch on large displays and at fast-forward.
- The GPU device starts on a background thread at launch, as compiling
  the shader takes about 130 ms; the first upscaled frame would otherwise
  wait for it.
- Upscaled pictures are display-only: emulation, states, screenshots and
  recordings are unchanged, and frame CRCs do not depend on the GPU.
- GPU tests skip themselves on a machine without Metal; the CPU filters'
  tests always run.
