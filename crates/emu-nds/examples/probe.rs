//! Runs a DS game headless and prints where both CPUs are each frame, to
//! see how far a game gets.
//!
//! Usage: `cargo run --release -p emu-nds --example probe -- game.nds
//! [frames] [screens.png]`. With a PNG path, the last frame's screens are
//! saved there, top above bottom.
//!
//! Environment variables:
//!
//! - `INPUT="120:a 130: 140:down 200:touch=128,96"` holds buttons from a
//!   frame on: here A from frame 120, nothing from 130, Down from 140, and
//!   a touch at (128, 96) on the bottom screen from 200. Join several with
//!   `+`, as in `300:a+touch=10,20`. Frames count from the start of this
//!   run, even after `STATE_IN`.
//! - `SNAP_EVERY=n` with `SNAP_DIR=dir` saves the screens every `n` frames
//!   as `dir/f000120.png` and so on.
//! - `SAVE_IN=game.sav` powers on with that battery save; `SAVE_OUT=path`
//!   writes the battery save there at the end.
//! - `GBA_ROM=game.gba` puts that GBA game in the GBA slot, with
//!   `GBA_SAVE_IN` and `GBA_SAVE_OUT` for its save as above.
//! - `STATE_IN=path` loads a save state before the first frame;
//!   `STATE_OUT=path` writes one at the end.
//! - `START_TIME=seconds` sets the real-time clock (Unix time) at power-on.
//! - `QUIET=1` prints the CPU line every 60 frames instead of every frame.
//! - `DUMP=hex` prints the ARM9 code there at the end instead of around
//!   its PC.

use std::{env, fs, io::BufWriter, path::Path, process};

use emu_core::{Button, Cartridge, Core, Input, Media, TouchPoint};
use emu_nds::Nds;

fn main() {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: probe game.nds [frames] [screens.png]");
        process::exit(2);
    };
    let frames: u32 = args.next().and_then(|n| n.parse().ok()).unwrap_or(60);
    let png_path = args.next();
    let rom = read_or_exit(&path);
    let save = env::var("SAVE_IN").ok().map(|path| read_or_exit(&path));
    let gba_rom = env::var("GBA_ROM").ok().map(|path| read_or_exit(&path));
    let gba_save = env::var("GBA_SAVE_IN").ok().map(|path| read_or_exit(&path));
    let media = Media {
        backup: save.as_deref(),
        start_time: env::var("START_TIME")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(0),
        second_slot: gba_rom.as_deref().map(|rom| Cartridge {
            rom,
            backup: gba_save.as_deref(),
        }),
        ..Media::new(&rom)
    };
    let mut nds = Nds::new(&media).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    if let Ok(path) = env::var("STATE_IN") {
        nds.load_state(&read_or_exit(&path))
            .unwrap_or_else(|error| {
                eprintln!("cannot load {path}: {error}");
                process::exit(1);
            });
    }
    let script = input_script();
    let snaps = snapshot_plan();
    let quiet = env::var_os("QUIET").is_some();
    for frame in 0..frames {
        let input = script
            .iter()
            .rev()
            .find(|(start, _)| *start <= frame)
            .map_or_else(Input::default, |(_, input)| *input);
        nds.run_frame(&[input], &mut Vec::new());
        if !quiet || frame % 60 == 59 {
            println!("frame={frame} {}", nds.debug_state());
        }
        if frame % 60 == 59 {
            println!("video frame={frame} {}", nds.debug_video());
        }
        if let Some((every, dir)) = &snaps
            && frame % every == every - 1
        {
            save_screens(&nds, &format!("{dir}/f{:06}.png", frame + 1));
        }
    }
    print_summary(&nds);
    if let Some(path) = png_path {
        save_screens(&nds, &path);
    }
    for (cartridge, variable) in [(0, "SAVE_OUT"), (1, "GBA_SAVE_OUT")] {
        if let Ok(path) = env::var(variable) {
            let mut bytes = Vec::new();
            nds.save_backup(cartridge, &mut bytes);
            write_or_exit(&path, &bytes);
        }
    }
    if let Ok(path) = env::var("STATE_OUT") {
        let mut bytes = Vec::new();
        nds.save_state(&mut bytes);
        write_or_exit(&path, &bytes);
    }
}

fn read_or_exit(path: &str) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|error| {
        eprintln!("cannot read {path}: {error}");
        process::exit(1);
    })
}

fn write_or_exit(path: &str, bytes: &[u8]) {
    fs::write(path, bytes).unwrap_or_else(|error| {
        eprintln!("cannot write {path}: {error}");
        process::exit(1);
    });
}

/// `INPUT` as (first frame, input held) pairs.
fn input_script() -> Vec<(u32, Input)> {
    let text = env::var("INPUT").unwrap_or_default();
    text.split_whitespace()
        .filter_map(|entry| {
            let (frame, names) = entry.split_once(':')?;
            Some((frame.parse().ok()?, parse_input(names)))
        })
        .collect()
}

/// One script entry's `+`-joined buttons and `touch=x,y`.
fn parse_input(names: &str) -> Input {
    let mut input = Input::default();
    for name in names.split('+').filter(|name| !name.is_empty()) {
        if let Some(point) = name.strip_prefix("touch=") {
            input.touch = parse_point(point);
        } else if let Ok(button) = name.parse::<Button>() {
            input.buttons.set(button, true);
        } else {
            eprintln!("unknown input {name:?}");
        }
    }
    input
}

fn parse_point(text: &str) -> Option<TouchPoint> {
    let (x, y) = text.split_once(',')?;
    Some(TouchPoint {
        x: x.parse().ok()?,
        y: y.parse().ok()?,
    })
}

/// `SNAP_EVERY` and `SNAP_DIR`, if both are set; creates the directory.
fn snapshot_plan() -> Option<(u32, String)> {
    let every: u32 = env::var("SNAP_EVERY").ok()?.parse().ok()?;
    let dir = env::var("SNAP_DIR").ok()?;
    if every == 0 {
        return None;
    }
    fs::create_dir_all(Path::new(&dir)).unwrap_or_else(|error| {
        eprintln!("cannot create {dir}: {error}");
        process::exit(1);
    });
    Some((every, dir))
}

/// Prints the BIOS calls, the result-line ink, the screens' colours, the
/// registers and the ARM9 code at the end of a run.
fn print_summary(nds: &Nds) {
    println!("{}", nds.debug_system_calls());
    // Test ROMs print a result line at the top left: count its ink.
    let top = &nds.screens()[0].pixels()[..256 * 8];
    let background = top[255];
    let ink = top.iter().filter(|pixel| **pixel != background).count();
    println!("result_line_ink={ink}");
    for (index, screen) in nds.screens().iter().enumerate() {
        let mut colors: Vec<u32> = screen.pixels().to_vec();
        colors.sort_unstable();
        colors.dedup();
        println!(
            "screen{index} distinct_colors={} first={:08X?}",
            colors.len(),
            &colors[..colors.len().min(4)]
        );
    }
    let [arm9, arm7] = nds.debug_registers();
    println!("arm9_registers={arm9:08X?}");
    println!("arm7_registers={arm7:08X?}");
    let word = |address: u32| {
        u32::from_le_bytes([0, 1, 2, 3].map(|byte| nds.peek(0, address + byte).unwrap_or(0)))
    };
    let pc = env::var("DUMP")
        .ok()
        .and_then(|text| u32::from_str_radix(text.trim_start_matches("0x"), 16).ok())
        .unwrap_or(arm9[15])
        & !3;
    for address in (pc.saturating_sub(0x40)..pc + 0x10).step_by(4) {
        println!("arm9_code {address:08X}: {:08X}", word(address));
    }
}

/// Writes both screens, top above bottom, as one RGB PNG.
fn save_screens(nds: &Nds, path: &str) {
    let mut rgb = Vec::new();
    for screen in nds.screens() {
        for pixel in screen.pixels() {
            let [blue, green, red, _] = pixel.to_le_bytes();
            rgb.extend([red, green, blue]);
        }
    }
    let file = fs::File::create(path).unwrap_or_else(|error| {
        eprintln!("cannot create {path}: {error}");
        process::exit(1);
    });
    let mut encoder = png::Encoder::new(BufWriter::new(file), 256, 384);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&rgb))
        .unwrap_or_else(|error| {
            eprintln!("cannot write {path}: {error}");
            process::exit(1);
        });
}
