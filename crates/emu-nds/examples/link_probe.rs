//! Runs two DS games headless, linked over local wireless, and prints
//! where each console's CPUs and radio are, to see how far a link session
//! gets.
//!
//! Usage: `cargo run --release -p emu-nds --example link_probe -- a.nds
//! b.nds [frames] [screens.png]`. With a PNG path, the last frame's screens
//! are saved there: each console's top above its bottom, the first console
//! on the left.
//!
//! Environment variables, as for `probe.rs` but per console (0 and 1):
//!
//! - `INPUT0` and `INPUT1` hold each console's buttons from a frame on, as
//!   in `"120:a 130: 140:down 200:touch=128,96"`; join several with `+`.
//!   Frames count from the start of this run.
//! - `SAVE_IN0`/`SAVE_IN1` power on with those battery saves;
//!   `SAVE_OUT0`/`SAVE_OUT1` write them at the end.
//! - `STATE_IN` loads a linked save state before the first frame;
//!   `STATE_OUT` writes one at the end.
//! - `START_TIME=seconds` sets both clocks (Unix time) at power-on;
//!   `START_TIME1` sets the second console's instead.
//! - `SNAP_EVERY=n` with `SNAP_DIR=dir` saves the screens every `n` frames
//!   as `dir/f000120.png` and so on.
//! - `QUIET=1` prints the state lines every 60 frames instead of every
//!   frame; the radios' lines come every 60 frames either way.

use std::{env, fs, io::BufWriter, path::Path, process};

use emu_core::{Button, Core, Input, Media, TouchPoint};
use emu_nds::Linked;

fn main() {
    let mut args = env::args().skip(1);
    let (Some(first), Some(second)) = (args.next(), args.next()) else {
        eprintln!("usage: link_probe a.nds b.nds [frames] [screens.png]");
        process::exit(2);
    };
    let frames: u32 = args.next().and_then(|n| n.parse().ok()).unwrap_or(60);
    let png_path = args.next();
    let roms = [read_or_exit(&first), read_or_exit(&second)];
    let saves =
        ["SAVE_IN0", "SAVE_IN1"].map(|name| env::var(name).ok().map(|path| read_or_exit(&path)));
    let start_time = |name: &str| env::var(name).ok().and_then(|text| text.parse().ok());
    let times = [
        start_time("START_TIME").unwrap_or(0),
        start_time("START_TIME1")
            .or_else(|| start_time("START_TIME"))
            .unwrap_or(0),
    ];
    let media: Vec<Media<'_>> = (0..2)
        .map(|index| Media {
            backup: saves[index].as_deref(),
            start_time: times[index],
            ..Media::new(&roms[index])
        })
        .collect();
    let mut linked = Linked::new(&media).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    if let Ok(path) = env::var("STATE_IN") {
        linked
            .load_state(&read_or_exit(&path))
            .unwrap_or_else(|error| {
                eprintln!("cannot load {path}: {error}");
                process::exit(1);
            });
    }
    let scripts = ["INPUT0", "INPUT1"].map(input_script);
    let snaps = snapshot_plan();
    let quiet = env::var_os("QUIET").is_some();
    for frame in 0..frames {
        let inputs = scripts.each_ref().map(|script| {
            script
                .iter()
                .rev()
                .find(|(start, _)| *start <= frame)
                .map_or_else(Input::default, |(_, input)| *input)
        });
        linked.run_frame(&inputs, &mut Vec::new());
        for player in 0..2 {
            let nds = linked.console(player);
            if !quiet || frame % 60 == 59 {
                println!("p{player} frame={frame} {}", nds.debug_state());
            }
            if frame % 60 == 59 {
                println!("p{player} wifi frame={frame} {}", nds.debug_wifi());
            }
        }
        if let Some((every, dir)) = &snaps
            && frame % every == every - 1
        {
            save_screens(&linked, &format!("{dir}/f{:06}.png", frame + 1));
        }
    }
    if let Some(path) = png_path {
        save_screens(&linked, &path);
    }
    for (cartridge, variable) in [(0, "SAVE_OUT0"), (1, "SAVE_OUT1")] {
        if let Ok(path) = env::var(variable) {
            let mut bytes = Vec::new();
            linked.save_backup(cartridge, &mut bytes);
            write_or_exit(&path, &bytes);
        }
    }
    if let Ok(path) = env::var("STATE_OUT") {
        let mut bytes = Vec::new();
        linked.save_state(&mut bytes);
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

/// The script in environment variable `name` as (first frame, input held)
/// pairs.
fn input_script(name: &str) -> Vec<(u32, Input)> {
    let text = env::var(name).unwrap_or_default();
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
            input.touch = point.split_once(',').and_then(|(x, y)| {
                Some(TouchPoint {
                    x: x.parse().ok()?,
                    y: y.parse().ok()?,
                })
            });
        } else if let Ok(button) = name.parse::<Button>() {
            input.buttons.set(button, true);
        } else {
            eprintln!("unknown input {name:?}");
        }
    }
    input
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

/// Writes the four screens as one RGB PNG: each console's top above its
/// bottom, side by side.
fn save_screens(linked: &Linked, path: &str) {
    let screens = linked.screens();
    let size = screens[0].size();
    let (width, height) = (usize::from(size.width), usize::from(size.height));
    let mut rgb = vec![0; 2 * width * 2 * height * 3];
    for (index, screen) in screens.iter().enumerate() {
        let (left, top) = ((index / 2) * width, (index % 2) * height);
        for (row, line) in screen.pixels().chunks_exact(width).enumerate() {
            for (column, pixel) in line.iter().enumerate() {
                let [blue, green, red, _] = pixel.to_le_bytes();
                let at = ((top + row) * 2 * width + left + column) * 3;
                rgb[at..at + 3].copy_from_slice(&[red, green, blue]);
            }
        }
    }
    let file = fs::File::create(path).unwrap_or_else(|error| {
        eprintln!("cannot create {path}: {error}");
        process::exit(1);
    });
    let mut encoder = png::Encoder::new(BufWriter::new(file), 2 * width as u32, 2 * height as u32);
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
