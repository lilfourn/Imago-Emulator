# Upscaling every game

Researched 2026-11-04. This covers how ds-emulator could draw every game
it runs at the resolution of the screen it is shown on, what other
emulators do, which algorithms the project may reuse, and what was
measured on this Mac. The requirements are in
[PRD-upscaler.md](../PRD-upscaler.md).

## Summary

- **"Higher resolution" means two different things.** A game that draws
  3D (DS, 3DS) can render more real detail at a higher internal
  resolution. A 2D game (GB, GBC, GBA, DS 2D, RPG Maker) has no more
  detail to give; an upscaler can only rebuild smooth edges from its
  pixels. Every game needs one or both, then an exact fit to the
  display's pixels.
- **The app already has the 3D half, partly.** The DS draws 3D at 1–8×
  (`Core::set_render_scale`), but the menu offers only 1–4×. The 3DS is
  capped at 4× because every frame is piped from the helper. Neither
  follows the display: the player picks a number.
- **The 2D half stops at Scale2x and Scale3x.** These CPU filters work
  at a fixed 2× or 3×. GPUI's linear filter covers the rest, up to the
  13–18× a Game Boy screen needs on these displays. Edges stay
  staircased.
- **The algorithms worth having are MIT-licensed shaders.** MMPX, Lior
  Halphon's OmniScale and HQ2x, Hyllian's xBR and Super-xBR, and
  Sp00kyFox's ScaleFX are all MIT. HQx, Eagle/2xSaI, Mazzoleni's Scale2x
  and xBRZ are GPL or LGPL, and so are Azahar's own copies of these
  filters. AMD FSR 1 and NVIDIA NIS are MIT; Apple's MetalFX is a
  system framework.
- **They need the GPU.** OmniScale and the xBR family run per output
  pixel, and filling a 240 Hz external display takes about 7.5 million
  output pixels per Game Boy frame. On the CPU, xBR costs about
  71 ns per output pixel (MMPX paper), about half a second a frame.
  That meets decision 0005's condition for revisiting it: a wanted
  filter that cannot run in budget on the CPU.
- **A Metal pass costs little, even with a round trip.** On this
  M5 Max, uploading a frame, upscaling it on the GPU and reading it back
  took 0.15–0.9 ms per frame up to about 5 million output pixels.
  Filling the 240 Hz external display, about 7.5–10 million, took
  1.1–1.6 ms. MetalFX's spatial scaler took 0.04–0.6 ms of GPU time.
- **GPUI is the constraint.** GPUI 0.2.2 samples every image linearly,
  has no shader hook, and its only zero-copy surface takes lossy 4:2:0
  YCbCr. So the upscaled frame still goes back to the CPU and into
  GPUI's atlas as an image. Upstream PRs for BGRA surfaces and nearest
  sampling are open but not merged.
- **MetalFX is right for 3D and painted art, wrong for pixel art.** On a
  Game Boy test image it kept the staircases and put halos round text.
  It is a good finish for DS and 3DS frames already at high internal
  resolution, and for RPG Maker's painted art.

## What the app does today

| Stage | Where | What |
|---|---|---|
| 3D internal resolution | `emu-nds` | 1–8× (`gpu3d::MAX_SCALE`), drawn in bands on threads. 2D layers are recomposed around the larger 3D, each 2D pixel spread over a square (`gpu2d/scaled.rs`). The menu and settings window offer 1–4×; the settings file takes 1–8. |
| 3D internal resolution | `emu-3ds` | `citra_resolution_factor`, capped at 4 (`host.rs` `MAX_SCALE`): every frame is read back from Vulkan and piped to the app, 11 MB a frame at 4×. |
| Colour correction, rotation | `emu-app` | CPU passes (`color.rs`, `video::turn`). |
| Filter | `emu-app/src/filter.rs` | LCD grid, scanlines (shading), Scale2x, Scale3x (fixed factors, skipped on frames with upscaled 3D). |
| Pre-scale | `emu-app/src/video.rs` | Nearest-neighbour by the largest whole factor that fits, at most 6×. GPUI's linear filter covers the rest ("sharp bilinear"). |
| Draw | `view.rs` | One `RenderImage` per placement per frame, at a rect in logical pixels that is not snapped to device pixels. GPUI scales it to device pixels without rounding (`Window::paint_image`), so even an image of the right size is resampled when its rect sits between pixels. |

RPG Maker frames come from `Graphics.snap_to_bitmap` and `raw_data` at
the game's own size (`emu-mkxp/src/bridge.rb`). Nothing upscales them
but the pre-scale.

## Screen types

"Every screen type" covers the displays a window can be on and the
emulated screens it shows. Device pixels per console pixel, for a game
filling the display's height (DS and 3DS stacked):

| Display | Pixels (scale) | Refresh | GB 160×144 | GBA 240×160 | DS 256×384 | 3DS 400×480 | RPG Maker 512×384 |
|---|---|---|---|---|---|---|---|
| MacBook Pro built-in | 3024×1964 (2×) | 120 Hz ProMotion | 13.6× | 12.3× | 5.1× | 4.1× | 5.1× |
| This Mac's external | 4608×2592 (2×) | 240 Hz | 18× | 16.2× | 6.75× | 5.4× | 6.75× |
| 4K at "looks like 1080p" | 3840×2160 (2×) | 60 Hz | 15× | 13.5× | 5.6× | 4.5× | 5.6× |
| 1080p monitor | 1920×1080 (1×) | 60–144 Hz | 7.5× | 6.75× | 2.8× | 2.25× | 2.8× |

What follows from the table:

- **No fixed factor fits.** The same game needs 2.8× on one display and
  6.75× on another, and the factor is rarely whole. The output size has
  to come from each screen's rect in device pixels, recomputed when the
  window resizes, goes full screen, or moves to another display.
- **3D at display resolution is within reach.** The DS needs 5–7×, under
  its 8× cap. The 3DS needs 4–5.4× when stacked, just over its 4× cap,
  and far under Azahar's 18×.
- **1× displays show sub-pixel placement most.** With half-pixel rects,
  an exact-size image is still blurred. Rects must snap to device pixels.
- **"Scaled" display modes cannot be pixel-exact.** In a "More Space"
  mode macOS draws at twice the looks-like size and resamples that to the
  panel. Upscaled output then looks better than sharp pixels, not worse.
- **Refresh rate sets the work rate, not the frame rate.** The view
  draws when a new emulated frame arrives. At 3× fast-forward on the
  240 Hz display that is up to 180 frames a second, each needing its own
  upscale.
- **Emulated screens differ within one window.** The 3DS's screens are
  400 and 320 pixels wide. The hybrid layout shows one screen large and
  one small, and rotation turns each. RPG Maker games come in any size
  (480×320, 512×384, 544×416, 640×480). Each placement gets its own
  output size.

## Algorithms and licences

The project reuses only MIT, BSD, zlib, MPL-2.0 and Apache code (PRD
section 9). libretro's slang-shaders repo has no repo-wide licence; each
shader carries its own.

| Algorithm | Licence of the source to port | Scale | Cost | Notes |
|---|---|---|---|---|
| MMPX (McGuire & Gagiu 2021) | MIT ([paper supplement](https://casual-effects.com/research/McGuire2021PixelArt/McGuire2021PixelArt-supplement.zip), [slang](https://github.com/libretro/slang-shaders/blob/master/edge-smoothing/scalenx/shaders/mmpx.slang)); Rust crate `mmpx` 0.2.0, MIT | 2× (repeat for 4×) | 4.9 ns per output pixel on a CPU ([paper](https://jcgt.org/published/0010/02/04/paper.pdf)) | Built to keep pixel art's shapes and text. Cheap enough for the CPU. |
| OmniScale (Lior Halphon) | MIT ([SameBoy](https://github.com/LIJI32/SameBoy/blob/master/Shaders/OmniScale.fsh)) | Any factor, one pass | GPU | Used by SameBoy and mGBA. Any factor suits any display. |
| xBR lv2/lv3 (Hyllian) | MIT ([slang](https://github.com/libretro/slang-shaders/blob/master/edge-smoothing/xbr/shaders/xbr-lv2-standalone.slang)); Rust crate `xbr`, MIT, 2× only | 2–4× | 71 ns per output pixel on a CPU; 0.038 on a GPU | The FFmpeg-derived C version is LGPL; port the shader. |
| Super-xBR (Hyllian) | MIT ([slang](https://github.com/libretro/slang-shaders/blob/master/edge-smoothing/xbr/shaders/super-xbr/super-xbr-pass0.slang)) | 2× in 3 passes, then a jinc2 fit | GPU | Softer, suits 3D and painted art. |
| ScaleFX (Sp00kyFox) | MIT ([slang](https://github.com/libretro/slang-shaders/blob/master/edge-smoothing/scalefx/shaders/scalefx-pass0.slang)) | 3× in 5 passes (9× chained) | GPU | Highly regarded for pixel art. |
| HQ2x (Lior Halphon's) | MIT ([mGBA shaders](https://github.com/mgba-emu/mgba/tree/master/res/shaders/hq2x.shader)) | 2× | GPU | The original HQx is LGPL-2.1+; this is an independent MIT one. |
| HQx (Stepin) | LGPL-2.1+ | 2–4× | | Out. |
| Scale2x (Mazzoleni), Eagle, 2xSaI | GPL | | | Out. The app's Scale2x/3x were written from the published rules. |
| xBRZ | GPL-3.0 (its linking exception covers MAME only) | 2–6× | | Out, as before. Azahar and mkxp-z ship it inside their own GPL code. |
| AMD FSR 1 (EASU + RCAS) | MIT | 1–2× per axis | GPU | Expects anti-aliased input; poor on hard pixel edges. |
| NVIDIA Image Scaling | MIT | | GPU | Same class as FSR 1. |
| Apple MetalFX spatial | System framework, macOS 13+ | Any; created at 8×–13× here | 0.04–0.2 ms GPU (measured below) | Built for rendered 3D. |
| Apple MetalFX temporal, frame interpolator, denoiser | System framework | Temporal 1–3× on this Mac | | Need depth and motion vectors, which 2D frames lack. Interpolation adds latency and changes pacing. |
| Anime4K | MIT | 2× | GPU | Built for 1080p anime going to 4K. |
| Real-ESRGAN `realesr-animevideov3` | Code BSD-3; weights' licence unverified | 4× | About 7 TFLOP/s for two DS screens at 60 fps (estimate) | A 2.4 MB model. Possible on a Max chip, at a large power cost. waifu2x and Real-CUGAN (MIT code) are similar. |

## Other emulators

- **melonDS** renders 3D at up to 16× internal resolution. 2D stays at
  256×192 and is composited with the larger 3D. Its new OpenGL 2D
  renderer turns BG layers and sprites into textures so that each layer
  can be filtered before composition
  ([blog](https://melonds.kuribo64.net/comments.php?id=241)). This is the
  right model for the DS: filter 2D, never the composited frame.
- **DeSmuME** has GPU scaling of 1–16× and xBRZ texture upscaling at 2×
  or 4× (OpenGL only). It cannot filter 2D, because its compositor works
  a scanline at a time.
- **Azahar** (the 3DS core the app hosts, 2126.2 and main):
  `citra_resolution_factor` goes to 18 (7200×4320).
  `citra_texture_filter` offers `none`, `Anime4K`, `Bicubic`,
  `ScaleForce`, `xBRZ` and `MMPX`. Two catches, checked in
  `src/citra_libretro/core_settings.cpp`:
  - The menu offers `xBRZ`, but the code only recognises
    `xBRZ freescale`, so choosing xBRZ gives no filter. Our host answers
    `GET_VARIABLE` itself, so it can send the value that works.
  - Anime4K is OpenGL only, and the app runs Vulkan.

  `citra_texture_sampling` (`GameControlled`, `NearestNeighbor`,
  `Linear`) and `citra_custom_textures` exist too.
- **mGBA** (MPL-2.0) ships MMPX, OmniScale, HQ2x (Halphon), xBR lv2/lv3
  and Scale2x/4x shaders: the MIT set above.
- **SameBoy** (Expat outside its `iOS/` folder) draws through Metal with
  OmniScale, HQ2x and Scale2x/4x.
- **OpenEmu-Shaders** runs slang presets on Metal through glslang and
  SPIRV-Cross. Its licence is unclear: no LICENSE file, and one file says
  "All rights reserved".
- **Dolphin** and some DS forks load HD texture packs. That replaces
  art, which needs packs per game; not upscaling.

## RPG Maker (mkxp-z)

- mkxp-z's `smoothScaling` (nearest, bilinear, bicubic, Lanczos3, xBRZ)
  applies only when it draws its own window. The app reads frames through
  `snap_to_bitmap`, so they never get it, and its xBRZ is GPL anyway.
- `enableHires` with `framebufferScalingFactor` draws the screen N×
  larger. Fonts, rotated and zoomed sprites, and `Hires/` replacement
  art then gain real detail. But `Bitmap::getRaw` copies only the
  native-size pixels and logs "GAME BUG" when a hi-res copy exists
  (`src/display/bitmap.cpp`). Reading the larger frame needs another
  capture path, and it is unconfirmed that Ruby can reach it.
- Pokémon Essentials art is mostly drawn at twice its pixel size (16×16
  tiles drawn as 32×32), while its text is not. A pixel-art scaler sees
  2×2 blocks and does little; a smooth scaler is the safer default.

## GPUI (checked in the 0.2.2 source, and on Zed main by the research agent)

- Every sampler is linear (`shaders.metal`), in 0.2.2 and on main. PR
  [#57393](https://github.com/zed-industries/zed/pull/57393) adds
  `img().image_filter(ImageFilter::Nearest)`; open, with conflicts.
- No custom shaders. PRs to add effects were closed unmerged.
- `Window::paint_surface` takes a `CVPixelBuffer` and asserts
  `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange` (`metal_renderer.rs`
  `draw_surfaces`). That format halves colour resolution, which smears
  pixel art; any other format panics in 0.2.2. BGRA surfaces are PR
  [#61291](https://github.com/zed-industries/zed/pull/61291), open.
- `Window` implements `HasWindowHandle` and hands out its `NSView`, so
  the app could attach its own `CAMetalLayer`. That layer would sit over
  GPUI's drawing, hiding the Escape menu, notices and script overlays.
- Images go into a Metal atlas of up to 16384×16384, so even an 18×
  frame fits.
- Zed main moved frame pacing to `CAMetalDisplayLink` (PR #65472,
  merged 2026-10-29). It fixes `nextDrawable` blocking about 33 ms on
  M5 Macs under macOS 26+. That is worth knowing for PRD question Q2,
  apart from upscaling.

## What was tested

On this Mac (Apple M5 Max, macOS 27.0.1, Metal 4), with a Swift program
compiled `-O`.

**Metal round trip.** For each size, 370 frames: upload the frame to a
shared texture, upscale on the GPU, copy the result into a shared buffer,
wait, and copy it into a fresh array as GPUI's `RenderImage` would need.
Times are p50 / p99 per frame.

| Input → output | Nearest kernel, round trip | MetalFX spatial, GPU only | MetalFX spatial, round trip |
|---|---|---|---|
| DS 256×192 → 1280×960 (5×) | 0.27 / 0.68 ms | 0.10 / 0.44 ms | 0.34 / 0.72 ms |
| DS 256×192 → 2048×1536 (8×) | 0.44 / 1.20 ms | 0.16 / 0.74 ms | 0.55 / 1.14 ms |
| GBA 240×160 → 1920×1280 (8×) | 0.41 / 1.14 ms | 0.16 / 0.55 ms | 0.49 / 0.97 ms |
| GBA 240×160 → 2880×1920 (12×) | 0.86 / 1.21 ms | 0.26–0.28 / 0.41–0.45 ms | 0.87–0.96 / 1.06–1.09 ms |
| GB 160×144 → 2080×1872 (13×) | 0.54 / 1.84 ms | 0.19 / 0.83 ms | 0.66 / 1.31 ms |
| GB 160×144 → 2880×2592 (18×, external) | 1.11–1.27 / 1.25–1.63 ms | 0.34–0.38 / 0.51–0.75 ms | 1.08–1.27 / 1.24–1.64 ms |
| GBA 240×160 → 3840×2560 (16×, external) | 1.42–1.46 / 1.65–1.82 ms | 0.49–0.58 / 0.74–0.80 ms | 1.49–1.62 / 1.75–1.77 ms |
| RPG Maker 512×384 → 1536×1152 (3×) | 0.37 / 0.87 ms | 0.13 / 0.44 ms | 0.43 / 0.79 ms |
| 3DS top 400×240 → 1600×960 (4×) | 0.32 / 0.81 ms | 0.12 / 0.53 ms | 0.40 / 0.80 ms |

Rows with ranges ran twice. The first run's MetalFX at 12× took
3.7 ms (p50) and did not happen again in two more runs. Copying 7–39 MB
frames on the CPU took 0.1–0.6 ms (p50). So the round trip grows with
the output, and filling the external display costs 1.1–1.6 ms a frame
before GPUI's own work. Not measured, and the main unknown: GPUI
creating and uploading an atlas texture of that size every frame inside
the app.

**MetalFX on pixel art.** `cgb-acid2`'s reference image (160×144,
freely redistributable) at 4×. Diagonals stayed staircased, flat colours
stayed flat, and text ("HELLO WORLD") came out soft with light halos.
MetalFX is a resampler for rendered content, not a pixel-art
reconstructor.

**Limits.** MetalFX's spatial scaler was created at every factor tried,
up to 13×. The temporal scaler reports 1.0–3.0× on this Mac.

## Where the upscaler could run

| Option | For | Against |
|---|---|---|
| A. CPU, more `filter.rs` passes (MMPX, the `xbr` crate) | No new technology; testable in CI | Fixed 2×/3×/4×, so it cannot fit any display; the any-factor shaders are 100× over budget |
| B. Metal compute in `emu-app`, read back, drawn as a GPUI image | Any algorithm at any output size; 0.2–0.8 ms measured; GPUI untouched; overlays and menus keep working | `unsafe` Metal calls; a copy back and an atlas upload per frame |
| C. B, then a GPUI patch for BGRA `paint_surface` | No copies: the GPU output is drawn as is; frees large displays at 240 Hz | Carrying a GPUI patch until upstream merges #61291 |
| D. A `CAMetalLayer` of our own in GPUI's `NSView` | Full control, lowest latency | Covers GPUI's menus, notices and script overlays; two layers to keep in step while resizing |

B is the way in. C follows if B's atlas upload turns out to cost too
much, and it also opens the way to taking 3DS frames from the helper
through shared memory without copying them. D is out until GPUI can
order native views (PR #62379).

## Plan

See [PRD-upscaler.md](../PRD-upscaler.md). In short:

1. A Metal pass in the app sizes every screen to its exact device pixels
   on every display. It offers a pixel-art upscaler for 2D and MetalFX
   for smooth content, and falls back to today's path without Metal.
2. 3D resolution gains an Auto setting that follows the display. The
   DS's 2D gets the pixel-art upscaler beside upscaled 3D, and the 3DS's
   texture filter is offered.
3. Larger 3DS resolutions through shared memory, and zero-copy drawing
   if the measurements call for it.
4. Later: DS texture upscaling, RPG Maker's hi-res framebuffer, and a
   neural upscaler experiment.
