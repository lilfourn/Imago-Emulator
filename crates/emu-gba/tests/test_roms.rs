//! Runs jsmolka's GBA test ROMs (fetch them with
//! `scripts/fetch-test-roms.sh`; without them this test prints a note and
//! passes).
//!
//! Each ROM runs its tests, leaves the number of the first failure in R12
//! (0 when all pass), and parks in an infinite `B .` loop. A ROM passes if
//! it parks with R12 = 0. `test-roms-passing.txt` lists ROMs that must keep
//! passing; record new ones with `UPDATE_PASSING=1`. Results go to
//! `target/gba-test-roms.txt`, and the screen of each failing ROM to
//! `target/gba-test-roms/`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use emu_core::{Core, Media};
use emu_gba::Gba;

const CYCLES_PER_SECOND: u64 = 16_777_216;
/// Steps with an unchanged PC that mean the ROM is parked.
const PARKED_STEPS: u32 = 64;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-roms/gba-tests")
}

fn target(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(name)
}

/// ROMs that report through R12.
const CASES: [&str; 9] = [
    "arm/arm.gba",
    "thumb/thumb.gba",
    "memory/memory.gba",
    "bios/bios.gba",
    "nes/nes.gba",
    "save/none.gba",
    "save/sram.gba",
    "save/flash64.gba",
    "save/flash128.gba",
];

fn run(path: &str) -> Result<(), String> {
    let rom = fs::read(root().join(path)).map_err(|error| format!("cannot read: {error}"))?;
    let mut gba = Gba::new(&Media::new(&rom)).map_err(|error| error.to_string())?;
    let mut last_pc = 0;
    let mut parked = 0;
    while gba.cycles() < 10 * CYCLES_PER_SECOND {
        gba.step();
        let pc = gba.cpu().reg(15);
        parked = if pc == last_pc { parked + 1 } else { 0 };
        last_pc = pc;
        if parked >= PARKED_STEPS {
            // Let the result reach the screen before judging.
            for _ in 0..2 {
                gba.run_frame(&[], &mut Vec::new());
            }
            return match gba.cpu().reg(12) {
                0 => Ok(()),
                failed => Err(format!("test {failed} failed")),
            };
        }
    }
    Err(format!("never parked; PC = {:08x}", gba.cpu().reg(15)))
}

fn write_screen(gba_screen: &emu_core::Frame, path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().expect("has a parent"))?;
    let size = gba_screen.size();
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(fs::File::create(path)?),
        u32::from(size.width),
        u32::from(size.height),
    );
    encoder.set_color(png::ColorType::Rgb);
    let rgb: Vec<u8> = gba_screen
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let [b, g, r, _] = pixel.to_le_bytes();
            [r, g, b]
        })
        .collect();
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

/// Runs `path` for a few seconds and saves its screen, for ROMs judged by
/// eye.
fn snapshot(path: &str) {
    let Ok(rom) = fs::read(root().join(path)) else {
        return;
    };
    let Ok(mut gba) = Gba::new(&Media::new(&rom)) else {
        return;
    };
    for _ in 0..120 {
        gba.run_frame(&[], &mut Vec::new());
    }
    let name = path.replace('/', "-").replace(".gba", ".png");
    write_screen(&gba.screens()[0], &target("gba-test-roms").join(name)).ok();
}

#[test]
fn test_roms() {
    if !root().exists() {
        eprintln!("skipping: run scripts/fetch-test-roms.sh to download the test ROMs");
        return;
    }
    let mut report = String::new();
    let mut passing = Vec::new();
    for path in CASES {
        match run(path) {
            Ok(()) => {
                passing.push(path);
                writeln!(report, "PASS {path}")
            }
            Err(why) => {
                snapshot(path);
                writeln!(report, "FAIL {path}: {why}")
            }
        }
        .expect("writing to a String");
    }
    for path in ["ppu/hello.gba", "ppu/shades.gba", "ppu/stripes.gba"] {
        snapshot(path);
    }
    fs::write(target("gba-test-roms.txt"), &report).expect("writing the report");
    println!("{report}");

    let ratchet = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test-roms-passing.txt");
    if std::env::var_os("UPDATE_PASSING").is_some() {
        fs::write(&ratchet, passing.join("\n") + "\n").expect("writing the ratchet");
    }
    let expected = fs::read_to_string(&ratchet).unwrap_or_default();
    let regressions: Vec<&str> = expected
        .lines()
        .filter(|line| !line.is_empty() && !passing.contains(line))
        .collect();
    assert!(regressions.is_empty(), "regressions: {regressions:?}");
}
