//! Runs a DS game headless and prints where both CPUs are each frame, to
//! see how far a game gets.
//!
//! Usage: `cargo run --release -p emu-nds --example probe -- game.nds [frames]`

use std::{env, fs, process};

use emu_core::{Core, Media};
use emu_nds::Nds;

fn main() {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: probe game.nds [frames]");
        process::exit(2);
    };
    let frames: u32 = args.next().and_then(|n| n.parse().ok()).unwrap_or(60);
    let rom = fs::read(&path).unwrap_or_else(|error| {
        eprintln!("cannot read {path}: {error}");
        process::exit(1);
    });
    let mut nds = Nds::new(&Media::new(&rom)).unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    });
    for frame in 0..frames {
        nds.run_frame(&[], &mut Vec::new());
        println!("frame={frame} {}", nds.debug_state());
    }
}
