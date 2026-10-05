# 0005 — Video filters run on the CPU; DS upscaling belongs to the core

- Status: proposed (settles PRD open question Q3; awaiting the owner's
  review)
- Date: 2026-10-05
- Deciders: Claude, for Luke Fournier's review

## Context

GPUI 0.2.2 samples images linearly and has no custom-shader hook
([decision 0001](0001-rust-and-gpui.md)). The research report's Metal
shader chain (LCD grid, CRT, xBRZ, slang presets) and the DS's upscaled 3D
need a way to run per-pixel work between the core and the screen. PRD
question Q3 asked whether to add a separate Metal or wgpu layer beside the
GPUI window, or to add a sampling option and shader hook to GPUI upstream.

By M11 the frontend has two such passes, both on the CPU:
- integer pre-scaling for sharp bilinear (M3);
- GBC/GBA colour correction, a 32,768-entry lookup table (M11).
At these systems' resolutions (at most 256×192 per screen) each pass
touches under 100,000 pixels a frame and costs well under a millisecond.

## Decision

1. **Filters are CPU passes** from one `Frame` to another, in `emu-app`,
   run before the frame becomes a GPUI image. New 2D filters (LCD grid,
   scanlines, xBRZ at small scale factors) join the same way.
2. **DS 3D upscaling is the core's job.** When the software rasterizer
   (M14) gains an internal-resolution setting, the DS core emits larger
   frames; the frontend keeps drawing images and never needs to know.
3. **No separate Metal/wgpu layer and no GPUI fork for now.**

Revisit if a wanted filter cannot run in budget on the CPU (the PRD gives
the UI thread 4 ms a frame), or if GPUI ships nearest-neighbour sampling or
a shader hook.

## Consequences

- One rendering path, no native-layer compositing, no extra GPU API. GPUI
  code stays confined to `emu-app`.
- slang shader presets and heavy CRT shaders are out of scope until the
  decision is revisited.
- Upscaled DS frames cost more CPU, both in the rasterizer and in the
  image upload. Plan the M14 renderer with that in mind, for example by
  rasterizing on a worker thread.
