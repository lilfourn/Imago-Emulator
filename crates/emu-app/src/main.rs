//! ds-emulator: a Game Boy, Game Boy Advance and Nintendo DS emulator for
//! macOS, built on GPUI.
//!
//! Usage: `ds-emulator [ROM]`

mod app;
mod bindings;
mod emulator;
mod layout;
mod persist;
mod video;
mod view;

fn main() {
    let rom = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    app::run(rom);
}
