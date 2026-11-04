# ds-emulator — Upscaler

| | |
|---|---|
| Status | Draft 0.1 |
| Date | 2026-11-04 |
| Owner | Luke Fournier |
| Research | [research/upscaling.md](research/upscaling.md) |
| Related | [PRD.md](PRD.md) UI-3, UI-4, NDS-9, 3DS-3 · [decision 0005](decisions/0005-video-filters-on-the-cpu.md) (revisited here) · [decision 0011](decisions/0011-3ds-games.md) |

## 1. Summary

One setting makes every game the app runs draw at the resolution of the
screen it is shown on. That covers Game Boy, Game Boy Color, GBA, DS, 3DS
and RPG Maker, on every display: the built-in ProMotion panel, the
240 Hz external monitor, a 1× monitor, in a window or full screen, in
every layout.

The upscaler does two jobs:

1. **Render resolution.** Games that draw 3D (DS, 3DS) render at the
   display's resolution, which adds real detail. The internal-resolution
   machinery for this already exists. What is new is that it follows the
   display, and that the 3DS goes past 4×.
2. **Reconstruction.** For 2D, and for 3D that cannot render that large,
   a GPU upscaler rebuilds smooth edges to exactly the screen's device
   pixels. Pixel-art games use an edge-directed pixel-art scaler; painted
   and 3D content uses MetalFX.

It runs as a Metal compute pass inside `emu-app`. On the CPU, the
pixel-art algorithms that work at any factor would take about half a
second per frame at the 12–18× a large display needs. That meets
decision 0005's own condition for revisiting it.

## 2. Background

What exists ([research](research/upscaling.md#what-the-app-does-today)):

- **Sharp bilinear:** nearest-neighbour enlargement by up to 6×, then
  GPUI's linear filter for the rest.
- **CPU filters:** Scale2x and Scale3x at fixed factors, plus the LCD
  grid and scanlines.
- **DS 3D:** 1–8× in the core, but only 1–4× in the menus. The 2D layers
  are enlarged as squares around the larger 3D.
- **3DS:** 1–4×, limited by piping each frame from the helper.
- **RPG Maker:** frames at the game's own size, nothing more.

What is missing:

- On the displays used, a game needs 2.3–18 device pixels per console
  pixel, rarely a whole number. Past 2× or 3×, 2D edges are staircases
  blurred by GPUI's linear filter.
- The 3D resolution is a number the player must pick and re-pick for
  each display.
- Beside upscaled 3D, the DS's 2D (menus, text, sprites, the whole bottom
  screen) stays blocky. The app's filters skip those frames altogether.
- Screen rects are not snapped to device pixels, so even an image of the
  right size is resampled half a pixel off.

## 3. Goals and non-goals

| ID | Goal |
|---|---|
| UG-1 | **Every game.** GB, GBC, GBA, DS, 3DS and RPG Maker games are drawn at the resolution of the screen they are shown on. |
| UG-2 | **Every screen type.** Correct on every display (1× and 2×, any size, 60–240 Hz and variable refresh, scaled modes), in a window or full screen, after moving between displays. Correct for every emulated screen and layout: stacked, side by side, single, hybrid, rotated, with gaps, with screens of different sizes. |
| UG-3 | **Free in play.** Frame pacing, input latency, emulation speed and 3× fast-forward are no worse than with upscaling off. |
| UG-4 | **One choice.** A good default for each system, and one control to change it. No per-game tuning needed. |
| UG-5 | **Display only.** Emulation, save states, rewind, frame CRCs, touch, Lua scripts, screenshots, recordings and headless runs are untouched. |
| UG-6 | **Clean licences.** Only MIT, BSD, zlib, MPL-2.0 or Apache code is ported, and no GPL shaders (PRD section 9). |

Non-goals:

- HD texture packs and replacement art, which are per-game work.
- Frame interpolation or generation. It adds latency and changes pacing
  (decision 0003).
- A slang preset loader or user-written shaders (still out, as in
  decision 0005).
- Upscaling the app's own UI, library art or the ambient backdrop.
- HDR output.
- Upscaled recordings. Recordings stay at native size.
- Windows and Linux.

## 4. Scenarios

1. Platinum, stacked, full screen on the MacBook. 3D renders at about 5×
   to match the panel. The bottom screen's menus and the top screen's 2D
   text get the pixel-art upscaler. Nothing about pacing changes at 120 Hz
   or at 3× fast-forward.
2. Emerald full screen on the 240 Hz external display. 240×160 fills
   3888×2592 pixels (16.2×) with smooth diagonals and readable text.
3. An RPG Maker fan game in a window, dragged from the MacBook to the
   external display. The picture re-fits at the new size as it arrives,
   with no restart and no setting touched.
4. A 3DS game stacked on the external display. 3D renders at 5× or more,
   past today's 4× cap.
5. Red on a 1080p 1× monitor. 160×144 fills 1080 lines at 7.5× with no
   half-pixel blur.
6. DS hybrid layout: a large top screen and a small bottom one, each
   upscaled to its own size.

## 5. Screen types

Device pixels per console pixel, for a game filling the display's height
(DS and 3DS stacked):

| Display | Pixels (scale) | Refresh | GB | GBA | DS | 3DS | RPG Maker 512×384 |
|---|---|---|---|---|---|---|---|
| MacBook Pro built-in | 3024×1964 (2×) | 120 Hz ProMotion | 13.6× | 12.3× | 5.1× | 4.1× | 5.1× |
| External | 4608×2592 (2×) | 240 Hz | 18× | 16.2× | 6.75× | 5.4× | 6.75× |
| 4K, "looks like 1080p" | 3840×2160 (2×) | 60 Hz | 15× | 13.5× | 5.6× | 4.5× | 5.6× |
| 1080p | 1920×1080 (1×) | 60–144 Hz | 7.5× | 6.75× | 2.8× | 2.25× | 2.8× |

The rules that make it work on all of them are the requirements UP-S1 to
UP-S9 in section 7.2.

## 6. Design

### 6.1 The path of a frame

```
core frame (native, or larger 3D)
  │  emulation thread publishes it (unchanged)
  ▼
video stage (new, off the UI thread), once per published frame:
  colour correction → rotation → upscale to each placement's device size
  → shading filter (LCD grid, scanlines), all in one Metal command buffer
  ▼
BGRA bytes per placement → GPUI RenderImage → drawn at a rect snapped
to device pixels
```

- **Each placement gets its own target size,** from its rect in logical
  pixels times the window's scale factor, rounded to whole device pixels.
  The view sends targets to the video stage when they change: a resize,
  full screen, a move to another display, or a layout, rotation, gap or
  swap change.
- **The video stage runs as frames are published, not in `render()`.**
  `refresh.rs` aims to have each frame ready 2 ms before its refresh. The
  upscale (0.2–1.6 ms measured) counts toward that ready time, so frames
  still meet their refresh.
- **Without Metal, today's CPU path runs unchanged:** correct, turn,
  filter, pre-scale. Its colour correction and filters stay as the
  reference the GPU versions are tested against.
- **Only frames that will be drawn are upscaled.** At fast-forward the
  view coalesces frames to the display's rate. The video stage takes
  only the latest frame, as `take_latest` does today.

### 6.2 Algorithms

| Mode | For | Algorithm | Why |
|---|---|---|---|
| Pixel Art | GB, GBC, GBA, DS 2D | Bake-off between OmniScale (one pass, any factor), ScaleFX (3×, 5 passes) and MMPX (2×, repeated), all MIT. The finish to the exact size is part of the pass (OmniScale) or a sharp resample. | Rebuild diagonals and curves, keep text and dithering legible. OmniScale leads because it renders straight to any output size. |
| Smooth | RPG Maker; 3D whose render resolution is below the display | MetalFX spatial scaler | Apple's own, 0.04–0.6 ms measured, no shader to maintain, any factor. Wrong for pixel art (halos, staircases), right for painted and rendered content. |
| Sharp Pixels | Anyone who wants the original look | Nearest-neighbour to the largest whole factor, then an area-weighted fit | Today's look, done exactly on the GPU. |

Shaders are written in Metal Shading Language, ported from the MIT
sources with their licence headers kept, and compiled at run time from
source. That keeps `cargo build` free of the Metal Toolchain download,
as GPUI's `runtime_shaders` does (PRD section 7, Build). No code comes
from Azahar's GPL copies of these filters, and no xBRZ.

Bake-off: run each candidate on freely redistributable frames (the GB
and GBA test ROMs' screens, store homebrew with permissive licences) and
on the owner's own games, locally. Score diagonal lines, curved sprites,
small text (5–8 px fonts), dithered gradients, UI boxes and 1-pixel
lines, plus cost at 18×. Images from commercial games are never
committed.

### 6.3 By system

| System | Render resolution | Picture on Auto | Notes |
|---|---|---|---|
| GB, GBC | none (2D) | Pixel Art | Colour correction (GBC) runs before the upscale, as now. |
| GBA | none (2D) | Pixel Art | Same. |
| DS | 3D at Auto: matches the display, capped at 8× | Pixel Art for 2D, the larger 3D as drawn | See 6.4. |
| 3DS | Auto: matches the display, cap raised past 4× by 6.5 | Smooth to finish | Optional texture filter, see 6.5. |
| RPG Maker | none for now (see 6.6) | Smooth | Essentials art is drawn at twice its pixel size, which pixel-art scalers handle badly. |

### 6.4 DS: 2D beside upscaled 3D

Upscaling the composed frame would run the pixel-art scaler over smooth
3D and smear it. melonDS filters 2D before composing it with 3D, and so
should we:

1. **Per screen, per frame (P1).** The core reports whether each screen
   showed 3D this frame. A screen that did not is upscaled from its
   native pixels with Pixel Art; `video::shrink` already recovers them
   exactly from an enlarged frame. That covers the bottom screen almost
   always, and every 2D game.
2. **Per pixel (P1).** For a screen that showed 3D, the core publishes,
   beside the larger frame, a native-size mask of the console pixels whose
   final colour came from the 3D layer, alone or blended
   (`gpu2d/scaled.rs` already works this out per pixel as `reaches_3d`
   and the layer chosen). The video stage takes the larger frame inside
   the mask and the Pixel Art upscale of the native frame outside it,
   blending across a one-pixel border.
3. **Per layer (P2).** Upscale each 2D layer before composing, as
   melonDS's new renderer does. This is the most correct, but a large
   change to the core.
4. **Textures (P2).** Upscale decoded 3D textures with MMPX (MIT crate
   `mmpx`) in the rasterizer, as DeSmuME does with xBRZ. Pokémon Black
   and White draw their battle sprites as textures.

Upscaled 3D costs the most CPU of anything here. The core measured about
3.2× full speed at 2× and 2.7× at 4× (White's bedroom, headless). At the
5–7× that Auto picks on these displays it may not hold 3× fast-forward.
So:

- The larger 3D is drawn only for frames the window will show: at
  fast-forward, not for frames the view will skip. Emulation and the
  native 3D the game reads are unchanged.
- Auto's cap is set from measurement, so that DS games hold 3×
  fast-forward on this Mac. It never changes during play, only when the
  display, window size or layout changes. Resolution that rises and falls
  on its own is the kind of self-adjusting behaviour the owner has
  rejected for layouts.

### 6.5 3DS: past 4×

- **Transport.** Frames move from the helper through shared memory, an
  IOSurface or POSIX shared memory mapped by both processes, instead of
  the stdin pipe. That removes the 11 MB-a-frame copy at 4× that sets
  today's cap. Raise the cap to 8× first, measured against 3×
  fast-forward; Azahar goes to 18×.
- **Texture filter.** Offer `citra_texture_filter` (`MMPX`, `ScaleForce`,
  `Bicubic`). For xBRZ the host must send `xBRZ freescale`, the value the
  core actually checks. Anime4K does nothing on Vulkan. The filter runs
  inside the GPL core the app downloads, so the app copies no GPL code.
  Off by default until the bake-off has looked at it.

### 6.6 RPG Maker

- **P1:** Smooth (MetalFX) on the captured native frame.
- **P2, a spike first:** mkxp-z's `enableHires` and
  `framebufferScalingFactor` render the screen N× larger. Text, which is
  TrueType, gains real sharpness, and so do zoomed and rotated sprites.
  `Bitmap::getRaw` returns only native pixels, so this needs a capture
  path for the larger framebuffer. Find out whether Ruby can reach it
  before planning more.

### 6.7 Controls

- **View > Picture** replaces View > Filter: **Auto** (the default),
  Sharp Pixels, Pixel Art, Smooth, LCD Grid, Scanlines. One choice,
  because a shading filter needs sharp pixels and an upscaler smooths
  them away. Auto picks per system as in 6.3.
- **View > 3D Resolution:** **Match Display** (the default), Native (1×),
  2×, 3×, 4×, 6×, 8×. The notice names what was picked, for example
  "3D at 5×, to match this display".
- **The settings window's Video section** gets the same two rows. The
  3D Resolution row today offers only 1–4×.
- **Settings file:** `[video] picture = "auto"` and
  `resolution_3d = "auto"` or `1`–`8`. Old `filter` values still load:
  `scale2x` and `scale3x` become Pixel Art, `lcd-grid` and `scanlines`
  stay, and `none` becomes Auto.
- Nothing is added to the game screen or the Escape menu (the owner keeps
  in-game chrome off the screens).

### 6.8 Where it runs, and why not elsewhere

From the [research](research/upscaling.md#where-the-upscaler-could-run):

- **A Metal compute pass in `emu-app`, read back and drawn as a GPUI
  image.** This is the plan. Any algorithm, any output size, and GPUI's
  menus, notices and script overlays keep drawing over the screens as
  they do now.
- **Then, if measurement calls for it, zero copy.** Patch GPUI so
  `paint_surface` accepts BGRA IOSurface-backed pixel buffers (upstream
  PR #61291 is open) and draw the GPU output as is. This decides
  whether 18× at 240 Hz fits. It also lets 3DS frames go from the helper
  to the screen without a CPU copy.
- **Not CPU passes:** fixed 2–4× factors cannot fit any display, and the
  any-factor algorithms cost about 100× the budget on the CPU.
- **Not a separate `CAMetalLayer`:** it would cover GPUI's menus and
  overlays.

Metal is reached through `objc2-metal` and `objc2-metal-fx` (0.3, MIT,
Apache-2.0 or zlib, the family the app already uses), in one module
marked `#[allow(unsafe_code)]` with a `// SAFETY:` comment on each
block, as `emu-3ds`'s Vulkan code is. That exception needs decision 0013
(PRD section 7, Code safety), which also records the change to decision
0005.

## 7. Requirements

Priority: **P0** is needed for the first release of the feature, **P1**
is wanted, **P2** comes later. Phases are in section 9.

### 7.1 Picture

| ID | Requirement | Priority · Phase |
|---|---|---|
| UP-1 | A Metal video stage in `emu-app` that colour corrects, turns, upscales and shades each published frame into one BGRA image per placement, at that placement's exact size in device pixels. | P0 · 1 |
| UP-2 | Pixel Art mode: the bake-off's winner (6.2), rendering straight to any output size, with diagonals and curves rebuilt and 5–8 px text still legible. | P0 · 1 |
| UP-3 | Smooth mode: MetalFX spatial scaler, to any output size. | P0 · 1 |
| UP-4 | Sharp Pixels, LCD Grid and Scanlines produce what today's CPU path produces, to within ±1 per channel. | P0 · 1 |
| UP-5 | Auto picks per system as in 6.3. It is the default; the default for 3D resolution is Match Display. | P0 · 1–2 |
| UP-6 | Without a Metal device, or if the video stage fails to start, today's CPU path runs, and a notice says so once. | P0 · 1 |
| UP-7 | View > Picture, View > 3D Resolution, the settings window rows and the settings file keys of 6.7, with old `filter` values migrated. | P0 · 1 |
| UP-8 | Shaders are ported only from MIT, BSD, zlib, MPL-2.0 or Apache sources, keep their licence headers, and compile from source at run time. | P0 · 1 |

### 7.2 Every screen type

| ID | Requirement | Priority · Phase |
|---|---|---|
| UP-S1 | Each placement's target is its rect × the window's current scale factor, rounded to whole device pixels. It is recomputed on resize, entering or leaving full screen, a move to another display, and a layout, rotation, gap or swap change, and the next frame drawn uses the new size. | P0 · 1 |
| UP-S2 | Placement rects snap to whole device pixels, origin and size, so an image of the target size is drawn 1:1. Touch mapping uses the same snapped rects. | P0 · 1 |
| UP-S3 | Correct at backing scale 1 and 2, at any window size, and in macOS's scaled display modes, where the target is the backing size. | P0 · 1 |
| UP-S4 | One upscale per frame drawn, never per display refresh. At 60, 120, 144 and 240 Hz and with ProMotion's variable refresh, frames still last the same whole number of refreshes as with upscaling off (decision 0003). | P0 · 1 |
| UP-S5 | A per-frame output budget in pixels, set from measurement so the stage stays within its time at 3× fast-forward on the 240 Hz display. Past it, the stage renders the largest size within budget and GPUI's linear filter covers the rest. | P0 · 1 |
| UP-S6 | Screens of different sizes in one window (3DS 400 and 320 wide; hybrid large and small; RPG Maker games of any size) each get their own target. | P0 · 1 |
| UP-S7 | Colours unchanged: sRGB in, sRGB out. Flat areas match upscaling off exactly on every display, wide-gamut and EDR panels included. | P0 · 1 |
| UP-S8 | No upscaling work while the window draws nothing (minimised, hidden or fully covered). | P1 · 1 |
| UP-S9 | Displays connected, disconnected or changed mid-game re-fit without a restart. | P1 · 1 |

### 7.3 3D and per-system

| ID | Requirement | Priority · Phase |
|---|---|---|
| UP-10 | Match Display for DS and 3DS 3D: the smallest whole factor at least the largest target's device pixels per console pixel, within the system's cap. Recomputed only when targets change, never during play. | P0 · 2 |
| UP-11 | DS menus and the settings window offer 1–8× (the core's range). | P0 · 2 |
| UP-12 | The DS's larger 3D is drawn only for frames the window will show. The 3D the game reads and the frame CRCs stay the same. | P1 · 2 |
| UP-13 | DS 2D beside upscaled 3D gets Pixel Art: per screen when no 3D showed, and per pixel through a 3D mask (6.4). | P1 · 2 |
| UP-14 | 3DS texture filter option (6.5), off by default, with the `xBRZ freescale` value sent for xBRZ. | P1 · 2 |
| UP-15 | 3DS frames through shared memory, and the 3DS cap raised to at least 8× while holding full speed at that scale. | P1 · 3 |
| UP-16 | Zero-copy drawing through a GPUI BGRA surface, if UP-S5's budget leaves the external display below its full size at 1× speed. | P1 · 3 |
| UP-17 | DS 2D layers upscaled before composing (melonDS's approach). | P2 · 4 |
| UP-18 | DS texture upscaling with MMPX. | P2 · 4 |
| UP-19 | RPG Maker hi-res framebuffer capture (6.6), after a spike. | P2 · 4 |
| UP-20 | A neural upscaler experiment (Real-ESRGAN `animevideov3`, waifu2x or ArtCNN through Core ML), only with clearly licensed weights, judged on quality per watt. | P2 · 4 |
| UP-21 | Optional upscaled screenshots, beside the native PNG. | P2 · 4 |
| UP-22 | A per-game Picture choice, saved beside the game as its bindings are. | P2 · 4 |

### 7.4 Untouched

| ID | Requirement | Priority |
|---|---|---|
| UP-N1 | Emulation, save states, rewind, frame CRCs, the headless runner and CI's determinism checks are identical with upscaling on and off. | P0 |
| UP-N2 | Screenshots and recordings stay at native size from the emulation thread, as now. | P0 |
| UP-N3 | Lua overlays, the ambient backdrop, notices and the Escape menu draw as now, over and around the screens. | P0 |
| UP-N4 | Touch and "touch at cursor" land on the same console pixel at every size and in every layout. | P0 |

## 8. Performance and acceptance

Budgets, on this Mac (M5 Max) in release builds:

| What | Budget |
|---|---|
| Video stage per frame, normal speed, built-in display full screen | ≤ 1 ms p50, ≤ 2 ms p99 |
| Video stage per frame, 3× fast-forward, 240 Hz external full screen | within UP-S5's budget; UI thread under 4 ms per frame (PRD section 7) |
| Frame pacing (`scripts/macos/bench.sh`, `bench-report.py --refresh-hz 120` and `240`) | frames drawn for an uneven number of refreshes no more than with upscaling off, within run-to-run noise |
| Frame start to window draw | no more than 1 ms later than with upscaling off |
| DS 3D at Match Display | full speed at 1×; holds 3× fast-forward at the cap Auto uses |
| Memory | flat over a 60 s run at the largest target, as measured when the one-image-per-screen design was adopted |

Acceptance tests:

- **Unit:** target sizes for every row of the screen table and every
  layout. Rect snapping. Settings parsing and the old `filter` values.
  Match Display's factor.
- **GPU output:** for each mode on test-ROM frames, compared with stored
  checksums or the CPU reference within a tolerance. These tests skip
  themselves when there is no Metal device, as on a CI runner without a
  GPU; the CPU reference tests always run.
- **Determinism:** the existing frame-CRC and state tests unchanged with
  upscaling on (UP-N1).
- **In the app:** each system on the built-in and external displays,
  windowed and full screen, every layout and rotation. Move the window
  between displays during play. Check the screens with an isolated HOME
  and `scripts/macos/check-layout.sh`, using `cp -c` clones of saves
  (never the real ones).
- **Pacing:** `bench.sh` at normal speed and 3× fast-forward, before and
  after, on both displays.

## 9. Phases

1. **The video stage and 2D.** UP-1 to UP-8, UP-S1 to UP-S9, with
   decision 0013. GB, GBC, GBA, RPG Maker and DS frames without upscaled
   3D get Pixel Art or Smooth at display size. Measure GPUI's atlas
   upload at the largest targets and set UP-S5's budget.
2. **3D that follows the display.** UP-10 to UP-14: Match Display, the
   DS's 1–8× in menus, larger 3D only for shown frames, DS 2D beside 3D,
   the 3DS texture filter.
3. **Bigger and cheaper.** UP-15 and UP-16: 3DS shared memory and a
   higher cap; zero-copy drawing if phase 1's measurements call for it.
4. **Later.** UP-17 to UP-22.

## 10. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| GPUI's atlas upload of 20–40 MB images every frame is slow, or its atlas keeps growing | Missed refreshes on large displays; memory growth | Measure in phase 1. Budget per frame (UP-S5). Zero-copy surface (UP-16). Check memory stays flat over 60 s. |
| Pixel-art scalers mangle text, dithering or 1-pixel UI lines | Worse than sharp pixels in menus | The bake-off scores text and dithering. Sharp Pixels is one menu choice away. |
| High DS 3D resolution can't keep 3× fast-forward | Fast-forward slows | Upscaled 3D only for shown frames (UP-12); Auto's cap set from measurement. |
| The video stage delays frames past their refresh | Uneven pacing on ProMotion | It runs as frames are published, and `refresh.rs`'s 2 ms margin includes it. Pacing benchmarks gate each phase. |
| `unsafe` Metal code | Memory bugs | One module, `// SAFETY:` comments, decision 0013, tests under the GPU. |
| MetalFX behaves differently across macOS versions | Small visual differences | Display only (UP-N1); not part of any CRC. |
| Shader licences | Legal | Port only the MIT sources named in the research; keep headers; no Azahar or xBRZ code. |
| Carrying a GPUI patch for zero copy | Upgrade friction | Only if measured necessary; track upstream PR #61291 and drop the patch when it lands. |

## 11. Open questions

| ID | Question | Needed by |
|---|---|---|
| UQ-1 | Should Auto (upscaled) be the default for existing players, or should they opt in? The draft says default, since higher resolution for every game is the point. | Phase 1 |
| UQ-2 | Which Pixel Art algorithm wins the bake-off: OmniScale, ScaleFX or MMPX? | Phase 1 |
| UQ-3 | One View > Picture menu (this draft), or keep View > Filter and add View > Upscaling? | Phase 1 |
| UQ-4 | What cap should Match Display use for DS 3D, given 3× fast-forward? | Phase 2 |
| UQ-5 | For zero copy, patch GPUI 0.2.2, or move to a Zed revision that has BGRA surfaces once they merge? That would also bring the `CAMetalDisplayLink` pacing fix (PRD Q2). | Phase 3 |
| UQ-6 | Should RPG Maker games whose art is drawn at twice its pixel size get Pixel Art at the art's own pixel size, without spoiling their text? | Phase 4 |
