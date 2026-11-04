//! ds-emulator: a Game Boy, Game Boy Advance, Nintendo DS and Nintendo 3DS
//! emulator for macOS, built on GPUI.
//!
//! Usage: `ds-emulator [--script SCRIPT.lua] [ROM [LINKED_ROM | GBA_ROM]]`.
//! Two Game Boy or Game Boy Advance ROMs start linked by a cable; a GBA ROM
//! after a DS ROM goes in the DS's GBA slot. A Lua script runs alongside
//! the first game to start.
//!
//! A 3DS game plays in a helper process: this executable, started again
//! with `--3ds-helper` (see `emu_3ds::run_helper_if_asked`).

mod ambient;
mod app;
mod app_icon;
mod archive;
mod audio;
mod bindings;
mod boxart;
mod capture;
mod cheats;
mod chords;
mod color;
mod controls;
mod cursor;
mod debugger;
mod debugger_window;
mod emulator;
mod filter;
mod game_db;
mod game_menu;
mod gamepad;
mod icons;
mod key_guide;
mod key_routes;
mod latch;
mod layout;
mod library;
mod library_view;
mod memory_viewer;
mod meter;
mod microphone;
mod mov;
mod persist;
mod probe;
mod randomize_window;
mod randomizer;
mod recorder;
mod refresh;
mod resample;
mod rewind;
mod rewind_thread;
mod rom_info;
mod romhack;
mod save_writer;
mod scripting;
mod settings;
mod settings_window;
mod slots;
mod speed;
mod store;
mod store_view;
mod theme;
mod trace;
mod turbo;
mod ui;
mod video;
mod view;
mod window_chrome;

use std::ffi::OsString;
use std::path::PathBuf;

fn main() {
    // Before anything else: the helper must not start the app.
    if let Some(code) = emu_3ds::run_helper_if_asked() {
        std::process::exit(code);
    }
    match parse_args(std::env::args_os().skip(1)) {
        Ok((roms, script)) => app::run(roms, script),
        Err(error) => {
            eprintln!("ds-emulator: {error}");
            eprintln!("usage: ds-emulator [--script SCRIPT.lua] [ROM [LINKED_ROM | GBA_ROM]]");
            std::process::exit(2);
        }
    }
}

/// The ROMs to open and the script to run, from the command line.
fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<(Vec<PathBuf>, Option<PathBuf>), String> {
    let mut args = args.into_iter();
    let mut roms = Vec::new();
    let mut script = None;
    while let Some(arg) = args.next() {
        if arg == "--script" {
            let path = args.next().ok_or("--script needs a path")?;
            script = Some(PathBuf::from(path));
        } else {
            roms.push(PathBuf::from(arg));
        }
    }
    Ok((roms, script))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_script_can_come_before_or_after_the_roms() {
        let expected = (
            vec![PathBuf::from("red.gb"), PathBuf::from("blue.gb")],
            Some(PathBuf::from("hud.lua")),
        );
        assert_eq!(
            parse_args(args(&["--script", "hud.lua", "red.gb", "blue.gb"])),
            Ok(expected.clone())
        );
        assert_eq!(
            parse_args(args(&["red.gb", "blue.gb", "--script", "hud.lua"])),
            Ok(expected)
        );
        assert_eq!(
            parse_args(args(&["game.nds"])),
            Ok((vec![PathBuf::from("game.nds")], None))
        );
        assert!(parse_args(args(&["game.nds", "--script"])).is_err());
    }
}
