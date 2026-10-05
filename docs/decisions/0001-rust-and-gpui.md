# 0001 — Rust and GPUI instead of C++, SDL3 and Dear ImGui

- Status: accepted
- Date: 2026-10-05
- Deciders: Luke Fournier

## Context

The research report recommends C++20, CMake, SDL3 (window, input, audio),
Metal through SDL_GPU, and Dear ImGui, and calls Rust with Cargo "an
excellent alternative". Rust 1.96 is already installed on the development
machine; CMake is not.

## Decision

1. **Rust (2024 edition) and Cargo**, as a workspace with one crate per
   responsibility. `unsafe` is denied by default.
2. **GPUI** (Zed's UI framework, crates.io 0.2.2) for the window, menus,
   input events and drawing. It renders through Metal on macOS.
3. Because GPUI owns the window and the main-thread run loop, **SDL3 is not
   used**. Audio will use `cpal` (CoreAudio) and gamepads `gilrs`. SDL's
   event pump must run on the macOS main thread, which GPUI already
   occupies.
4. **Debugger and settings UI in GPUI** instead of Dear ImGui.
5. GPUI's **`runtime_shaders`** feature is enabled, so building does not need
   the separately downloaded Xcode Metal Toolchain.

## Consequences

- Memory safety and Cargo's tooling, at the cost of designing around the
  borrow checker. Each core owns its whole machine in one struct, and the
  CPU borrows the bus per call instead of holding a reference to it.
- GPUI has no nearest-neighbour sampling and no custom-shader hook in
  0.2.2. The app pre-scales frames on the CPU by an integer factor (sharp
  bilinear). The report's Metal shader chain (LCD grid, CRT, xBRZ, slang
  presets) and the DS's GPU 3D upscaling need either a separate Metal/wgpu
  layer composited with the GPUI window, or upstream GPUI changes. This is
  PRD open question Q3, to settle by M11.
- GPUI's Metal atlas reclaims space only when every image in a texture is
  dropped. The app streams frames as images and drops the previous one each
  frame, and keeps no other long-lived polychrome images. Memory was flat in
  testing; re-check after GPUI upgrades.
- GPUI is pre-1.0, and the crates.io release trails Zed's main branch. All
  GPUI code stays inside `emu-app`, and upgrades are deliberate.
- Mac-native menus and shortcuts with no extra work. Windows and Linux are
  possible later but untested.
- Shader compilation happens at app startup (`runtime_shaders`), which adds a
  little launch time. Installing the Metal Toolchain
  (`xcodebuild -downloadComponent MetalToolchain`) and dropping the feature
  would remove it.
