//! The contract between emulator cores and frontends.
//!
//! Every system (Game Boy, Game Boy Advance, Nintendo DS) implements [`Core`].
//! Frontends (the GPUI app and the headless runner) only ever talk to
//! `dyn Core`, so save states, rewind, fast-forward, remapping and shaders
//! are written once and work for every system.
//!
//! Everything crossing this boundary is plain data: pixels are `u32` words,
//! audio is interleaved `i16` stereo, input is a bitmask, and state is bytes.

mod audio;
mod core;
mod input;
mod media;
mod system;
pub mod test_pattern;
mod video;

pub use audio::StereoSample;
pub use core::{Backup, Core, CoreInfo, FrameRate, MemoryRegion, StateError};
pub use input::{Button, Buttons, Input, ParseButtonError, TouchPoint};
pub use media::{LoadError, Media, SystemFile, SystemFileKind};
pub use system::{ParseSystemError, System};
pub use video::{Frame, ScreenSize, rgb};
