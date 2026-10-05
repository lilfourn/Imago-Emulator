//! Prints the CPU state before every instruction, one line each:
//! address, opcode, mode, CPSR and R0–R15.
//!
//! Usage: `cargo run --release -p emu-gba --example trace -- ROM [SKIP] [COUNT]`
//! skips SKIP instructions, then prints COUNT.

use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use emu_core::Media;
use emu_gba::Gba;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: trace ROM [SKIP] [COUNT]");
        return ExitCode::from(2);
    };
    let skip: u64 = args.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let count: u64 = args.next().and_then(|n| n.parse().ok()).unwrap_or(1000);
    let rom = match std::fs::read(&path) {
        Ok(rom) => rom,
        Err(error) => {
            eprintln!("trace: cannot read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut gba = match Gba::new(&Media::new(&rom)) {
        Ok(gba) => gba,
        Err(error) => {
            eprintln!("trace: {error}");
            return ExitCode::FAILURE;
        }
    };
    for _ in 0..skip {
        gba.step();
    }
    let mut out = BufWriter::new(io::stdout().lock());
    for _ in 0..count {
        let cpu = gba.cpu();
        let width = if cpu.thumb() { 2 } else { 4 };
        let address = cpu.reg(15).wrapping_sub(2 * width);
        let mut line = format!(
            "{address:08x} {:08x} {:?} cpsr={:08x}",
            cpu.pipeline()[0],
            cpu.mode(),
            cpu.cpsr()
        );
        for index in 0..16 {
            line.push_str(&format!(" r{index}={:08x}", cpu.reg(index)));
        }
        if writeln!(out, "{line}").is_err() {
            break;
        }
        gba.step();
    }
    ExitCode::SUCCESS
}
