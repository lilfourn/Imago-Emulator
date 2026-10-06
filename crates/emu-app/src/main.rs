//! ds-emulator: a Game Boy, Game Boy Advance and Nintendo DS emulator for
//! macOS, built on GPUI.
//!
//! Usage: `ds-emulator [ROM [LINKED_ROM]]`. Two Game Boy or Game Boy
//! Advance ROMs start linked by a cable.

mod app;
mod audio;
mod bindings;
mod cheats;
mod color;
mod controls;
mod emulator;
mod gamepad;
mod layout;
mod memory_viewer;
mod persist;
mod resample;
mod rewind;
mod settings;
mod slots;
mod video;
mod view;

fn main() {
    let roms = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .collect();
    app::run(roms);
}
