//! The contract between emulator cores and frontends.
//!
//! Every system (Game Boy, Game Boy Advance, Nintendo DS) implements [`Core`].
//! Frontends (the GPUI app and the headless runner) only ever talk to
//! `dyn Core`, so save states, rewind, fast-forward, remapping and shaders
//! are written once and work for every system.
//!
//! Everything crossing this boundary is plain data: pixels are `u32` words,
//! audio is interleaved `i16` stereo, input is a bitmask, and state is bytes.
//!
//! Two modules are for the cores themselves, which share them: the
//! drawing behind the debugger's pictures of video memory
//! ([`debug::draw`], [`debug::gba_video`]) and what every linked pair of
//! consoles does the same way ([`pair`]).

mod audio;
mod core;
pub mod debug;
mod input;
mod media;
pub mod pair;
mod rom_patch;
mod system;
pub mod test_pattern;
mod video;

pub use audio::StereoSample;
pub use core::{Core, CoreInfo, FrameRate, MemoryRegion, StateError};
pub use debug::Debugger;
pub use input::{Button, Buttons, Input, ParseButtonError, TouchPoint};
pub use media::{Cartridge, DmgColors, LoadError, Media, SystemFile, SystemFileKind};
pub use rom_patch::RomPatch;
pub use system::{ParseSystemError, System};
pub use video::{Frame, ScreenSize, TextureUpscaling, rgb};
