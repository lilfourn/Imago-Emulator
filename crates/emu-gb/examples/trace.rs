//! Prints the CPU state before every instruction, one line each, in the
//! Gameboy Doctor format, so traces can be diffed against other emulators:
//!
//! ```text
//! A:01 F:B0 B:00 C:13 D:00 E:D8 H:01 L:4D SP:FFFE PC:0100 PCMEM:00,C3,13,02
//! ```
//!
//! Usage: `cargo run --release -p emu-gb --example trace -- ROM [INSTRUCTIONS]`

use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use emu_core::Media;
use emu_gb::Gb;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: trace ROM [INSTRUCTIONS]");
        return ExitCode::from(2);
    };
    let limit: u64 = args
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1_000_000);
    let rom = match std::fs::read(&path) {
        Ok(rom) => rom,
        Err(error) => {
            eprintln!("trace: cannot read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut gb = match Gb::new(&Media::new(&rom)) {
        Ok(gb) => gb,
        Err(error) => {
            eprintln!("trace: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut out = BufWriter::new(io::stdout().lock());
    let mut traced = 0;
    let mut print = |gb: &Gb| {
        let r = gb.registers();
        let mem = |offset| gb.peek(r.pc.wrapping_add(offset));
        writeln!(
            out,
            "A:{:02X} F:{:02X} B:{:02X} C:{:02X} D:{:02X} E:{:02X} H:{:02X} L:{:02X} SP:{:04X} PC:{:04X} PCMEM:{:02X},{:02X},{:02X},{:02X}",
            r.a,
            r.f,
            r.b,
            r.c,
            r.d,
            r.e,
            r.h,
            r.l,
            r.sp,
            r.pc,
            mem(0),
            mem(1),
            mem(2),
            mem(3),
        )
    };
    if print(&gb).is_err() {
        return ExitCode::SUCCESS;
    }
    while traced < limit {
        if gb.step().is_some() {
            traced += 1;
            // Stop quietly when the reader goes away (`| head`).
            if print(&gb).is_err() {
                break;
            }
        }
    }
    ExitCode::SUCCESS
}
