//! Runs a DS game headless and prints where both CPUs are each frame, to
//! see how far a game gets.
//!
//! Usage: `cargo run --release -p emu-nds --example probe -- game.nds
//! [frames] [screens.png]`. With a PNG path, the last frame's screens are
//! saved there, top above bottom.
//!
//! `INPUT="120:a 130: 140:down"` holds buttons from a frame on: here A
//! from frame 120, nothing from 130, Down from 140. `DUMP=hex` prints the
//! ARM9 code there at the end instead of around its PC.

use std::{env, fs, io::BufWriter, process};

use emu_core::{Button, Buttons, Core, Input, Media};
use emu_nds::Nds;

fn main() {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: probe game.nds [frames]");
        process::exit(2);
    };
    let frames: u32 = args.next().and_then(|n| n.parse().ok()).unwrap_or(60);
    let png_path = args.next();
    let rom = fs::read(&path).unwrap_or_else(|error| {
        eprintln!("cannot read {path}: {error}");
        process::exit(1);
    });
    let mut nds = Nds::new(&Media::new(&rom)).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    let script = input_script();
    for frame in 0..frames {
        let held = script
            .iter()
            .rev()
            .find(|(start, _)| *start <= frame)
            .map_or(Buttons::NONE, |(_, buttons)| *buttons);
        let input = Input {
            buttons: held,
            ..Input::default()
        };
        nds.run_frame(&[input], &mut Vec::new());
        println!("frame={frame} {}", nds.debug_state());
        if frame % 60 == 59 {
            println!("video frame={frame} {}", nds.debug_video());
        }
    }
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
    if let Some(path) = png_path {
        save_screens(&nds, &path);
    }
}

/// `INPUT` as (first frame, buttons held) pairs.
fn input_script() -> Vec<(u32, Buttons)> {
    let text = env::var("INPUT").unwrap_or_default();
    text.split_whitespace()
        .filter_map(|entry| {
            let (frame, names) = entry.split_once(':')?;
            let buttons = names
                .split('+')
                .filter(|name| !name.is_empty())
                .filter_map(|name| name.parse::<Button>().ok())
                .collect();
            Some((frame.parse().ok()?, buttons))
        })
        .collect()
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
