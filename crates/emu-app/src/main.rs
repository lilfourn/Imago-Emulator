//! ds-emulator: a Game Boy, Game Boy Advance and Nintendo DS emulator for
//! macOS, built on GPUI.
//!
//! Usage: `ds-emulator [ROM [LINKED_ROM | GBA_ROM]]`. Two Game Boy or Game
//! Boy Advance ROMs start linked by a cable; a GBA ROM after a DS ROM goes
//! in the DS's GBA slot.

mod app;
mod audio;
mod bindings;
mod cheats;
mod chords;
mod color;
mod controls;
mod cursor;
mod debugger;
mod debugger_window;
mod emulator;
mod filter;
mod gamepad;
mod latch;
mod layout;
mod memory_viewer;
mod microphone;
mod persist;
mod resample;
mod rewind;
mod settings;
mod slots;
mod speed;
mod turbo;
mod video;
mod view;

fn main() {
    let roms = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .collect();
    app::run(roms);
}
