//! Runs the c-sp/game-boy-test-roms bundle against the core.
//!
//! Fetch the ROMs first with `scripts/fetch-test-roms.sh`; without them this
//! test prints a note and passes.
//!
//! `test-roms-passing.txt` lists every ROM known to pass. A listed ROM that
//! fails is a regression and fails this test. ROMs that start passing are
//! reported; run with `UPDATE_PASSING=1` to record them. The full results go
//! to `target/gb-test-roms.txt`, one `PASS`/`FAIL` line per ROM.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use emu_core::{Button, Core, DmgColors, Input, Media};
use emu_gb::Gb;

const CYCLES_PER_SECOND: u64 = 4_194_304;

/// How a test ROM reports its result.
#[derive(Debug, Clone, Copy)]
enum Check {
    /// Blargg's serial output: "Passed" or "Failed".
    Serial,
    /// Blargg's memory protocol: signature DE B0 61 at 0xA001, status at
    /// 0xA000 (0x80 while running, 0x00 on success).
    BlarggMemory,
    /// Mooneye's protocol: `LD B, B` with B, C, D, E, H, L = 3, 5, 8, 13,
    /// 21, 34.
    Fibonacci,
    /// At `LD B, B`, or when time runs out, the screen must match this PNG
    /// next to the ROM.
    Screenshot(&'static str),
    /// Gambatte's: after [`GAMBATTE_CYCLES`], the screen's top row of
    /// tiles shows, in [`GAMBATTE_DIGITS`], the hex digits after the last
    /// `_out` in the file name. That is the CGB result, and the ROMs run
    /// here ask for a CGB.
    Gambatte,
    /// GBMicrotest's: after [`MICROTEST_CYCLES`], 0xFF82 holds 0x01 for
    /// a pass and 0xFF for a failure.
    Microtest,
}

/// How long Gambatte's test runner runs a ROM: 16 frames.
const GAMBATTE_CYCLES: u64 = 16 * 70_224;

/// Gambatte's font for the digits 0 to F, one byte per row, leftmost
/// pixel in bit 7; set bits are black, clear bits white (`testrunner.cpp`
/// in gambatte-core).
const GAMBATTE_DIGITS: [[u8; 8]; 16] = [
    [0x00, 0x7F, 0x41, 0x41, 0x41, 0x41, 0x41, 0x7F],
    [0x00, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08],
    [0x00, 0x7F, 0x01, 0x01, 0x7F, 0x40, 0x40, 0x7F],
    [0x00, 0x7F, 0x01, 0x01, 0x3F, 0x01, 0x01, 0x7F],
    [0x00, 0x41, 0x41, 0x41, 0x7F, 0x01, 0x01, 0x01],
    [0x00, 0x7F, 0x40, 0x40, 0x7E, 0x01, 0x01, 0x7E],
    [0x00, 0x7F, 0x40, 0x40, 0x7F, 0x41, 0x41, 0x7F],
    [0x00, 0x7F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
    [0x00, 0x3E, 0x41, 0x41, 0x3E, 0x41, 0x41, 0x3E],
    [0x00, 0x7F, 0x41, 0x41, 0x7F, 0x01, 0x01, 0x7F],
    [0x00, 0x08, 0x22, 0x41, 0x7F, 0x41, 0x41, 0x41],
    [0x00, 0x7E, 0x41, 0x41, 0x7E, 0x41, 0x41, 0x7E],
    [0x00, 0x3E, 0x41, 0x40, 0x40, 0x40, 0x41, 0x3E],
    [0x00, 0x7E, 0x41, 0x41, 0x41, 0x41, 0x41, 0x7E],
    [0x00, 0x7F, 0x40, 0x40, 0x7F, 0x40, 0x40, 0x7F],
    [0x00, 0x7F, 0x40, 0x40, 0x7F, 0x40, 0x40, 0x40],
];

/// How long a GBMicrotest ROM runs: two frames is enough for all of them.
const MICROTEST_CYCLES: u64 = 2 * 70_224;

struct Case {
    /// Name in the report and the ratchet: the path, plus the subtest for
    /// ROMs run more than once.
    label: String,
    /// Path relative to the bundle root.
    path: String,
    check: Check,
    timeout_seconds: u64,
    /// Buttons to tap one after another, starting one second in; for test
    /// ROMs with a menu.
    presses: &'static [Button],
    /// How a ROM made for the original Game Boy is run: on a DMG, or on a
    /// Game Boy Color in DMG mode.
    dmg_colors: DmgColors,
}

/// When scripted presses start, and how long each press and gap lasts.
const PRESS_START: u64 = CYCLES_PER_SECOND;
const PRESS_LENGTH: u64 = CYCLES_PER_SECOND / 8;

fn bundle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-roms/game-boy-test-roms")
}

fn roms_in(directory: &str) -> Vec<String> {
    roms_with_extension(directory, "gb")
}

/// Every file ending in `.{extension}` under `directory`, relative to the
/// bundle root, sorted.
fn roms_with_extension(directory: &str, extension: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![bundle_root().join(directory)];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == extension) {
                let relative = path
                    .strip_prefix(bundle_root())
                    .expect("under the bundle root");
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found
}

/// Whether a Mooneye ROM targets the DMG-ABC this core emulates. File names
/// end in a model suffix such as `-dmgABCmgb`, `-GS` or `-sgb`.
fn runs_on_dmg(path: &str) -> bool {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    match stem.rsplit_once('-') {
        None => true,
        Some((_, models)) => models.contains("dmgABC") || models.contains('G'),
    }
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let mut add = |paths: Vec<String>, check, timeout_seconds| {
        cases.extend(paths.into_iter().map(|path| Case {
            label: path.clone(),
            path,
            check,
            timeout_seconds,
            presses: &[],
            dmg_colors: DmgColors::Grey,
        }));
    };
    add(roms_in("blargg/cpu_instrs"), Check::Serial, 60);
    add(roms_in("blargg/instr_timing"), Check::Serial, 5);
    add(roms_in("blargg/mem_timing"), Check::Serial, 5);
    add(roms_in("blargg/mem_timing-2"), Check::BlarggMemory, 6);
    add(
        vec!["blargg/halt_bug.gb".to_owned()],
        Check::Screenshot("halt_bug-dmg-cgb.png"),
        2,
    );
    add(roms_in("blargg/dmg_sound"), Check::BlarggMemory, 40);
    add(roms_in("blargg/oam_bug"), Check::BlarggMemory, 25);
    let mooneye = roms_in("mooneye-test-suite/acceptance")
        .into_iter()
        .chain(roms_in("mooneye-test-suite/emulator-only"))
        .filter(|path| runs_on_dmg(path))
        .collect();
    add(mooneye, Check::Fibonacci, 120);
    add(
        vec!["dmg-acid2/dmg-acid2.gb".to_owned()],
        Check::Screenshot("dmg-acid2-dmg.png"),
        5,
    );
    add(
        vec!["cgb-acid2/cgb-acid2.gbc".to_owned()],
        Check::Screenshot("cgb-acid2.png"),
        5,
    );
    add(
        vec!["cgb-acid-hell/cgb-acid-hell.gbc".to_owned()],
        Check::Screenshot("cgb-acid-hell.png"),
        5,
    );
    add(roms_in("blargg/cgb_sound"), Check::BlarggMemory, 40);
    // HALT with an interrupt both requested and enabled, after EI or with
    // IME off: the HALT bug.
    let halt_bug = roms_with_extension("gambatte/halt", "gbc")
        .into_iter()
        .filter(|path| path.contains("ifandie"))
        .collect();
    add(halt_bug, Check::Gambatte, 1);
    // Writing STAT on a DMG raises a spurious STAT interrupt.
    let stat_write = roms_in("gbmicrotest")
        .into_iter()
        .filter(|path| path.contains("stat_write_glitch"))
        .collect();
    add(stat_write, Check::Microtest, 1);
    add(
        vec!["mbc3-tester/mbc3-tester.gb".to_owned()],
        Check::Screenshot("mbc3-tester-dmg.png"),
        1,
    );
    // On a Game Boy Color in DMG mode, with the palettes it gives games
    // it does not know.
    cases.push(Case {
        label: "dmg-acid2/dmg-acid2.gb (CGB in DMG mode)".to_owned(),
        path: "dmg-acid2/dmg-acid2.gb".to_owned(),
        check: Check::Screenshot("dmg-acid2-cgb.png"),
        timeout_seconds: 5,
        presses: &[],
        dmg_colors: DmgColors::ByTitle,
    });
    // rtc3test picks a subtest from a menu, then runs it for a fixed time.
    for (subtest, presses, png, seconds) in [
        (
            "basic",
            &[Button::A][..],
            "rtc3test-basic-tests-cgb.png",
            13,
        ),
        (
            "range",
            &[Button::Down, Button::A][..],
            "rtc3test-range-tests-cgb.png",
            8,
        ),
        (
            "sub-second writes",
            &[Button::Down, Button::Down, Button::A][..],
            "rtc3test-sub-second-writes-cgb.png",
            26,
        ),
    ] {
        cases.push(Case {
            label: format!("rtc3test/rtc3test.gb ({subtest})"),
            path: "rtc3test/rtc3test.gb".to_owned(),
            check: Check::Screenshot(png),
            timeout_seconds: seconds + 3,
            presses,
            dmg_colors: DmgColors::Grey,
        });
    }
    cases
}

/// The input a scripted case holds at `cycle`.
fn scripted_input(presses: &[Button], cycle: u64) -> Input {
    let mut input = Input::default();
    if let Some(elapsed) = cycle.checked_sub(PRESS_START) {
        let slot = elapsed / PRESS_LENGTH;
        // Even slots hold a button, odd slots are gaps between presses.
        if slot.is_multiple_of(2)
            && let Some(button) = usize::try_from(slot / 2).ok().and_then(|i| presses.get(i))
        {
            input.buttons.set(*button, true);
        }
    }
    input
}

/// Runs one ROM; `Ok(())` if it passes, otherwise why not.
fn run(case: &Case) -> Result<(), String> {
    let rom_path = bundle_root().join(&case.path);
    let rom = fs::read(&rom_path).map_err(|error| format!("cannot read: {error}"))?;
    let media = Media {
        dmg_colors: case.dmg_colors,
        ..Media::new(&rom)
    };
    let mut gb = Gb::new(&media).map_err(|error| error.to_string())?;
    let deadline = case.timeout_seconds * CYCLES_PER_SECOND;
    let mut next_poll = 0;

    let mut held = Input::default();
    while gb.cycles() < deadline {
        if !case.presses.is_empty() {
            let input = scripted_input(case.presses, gb.cycles());
            if input != held {
                gb.set_input(&input);
                held = input;
            }
        }
        let opcode = gb.step();
        match case.check {
            Check::Fibonacci | Check::Screenshot(_) if opcode == Some(0x40) => {
                return finish(case.check, &gb, &rom_path);
            }
            Check::Gambatte if gb.cycles() >= GAMBATTE_CYCLES => {
                return finish(case.check, &gb, &rom_path);
            }
            Check::Microtest if gb.cycles() >= MICROTEST_CYCLES => {
                return finish(case.check, &gb, &rom_path);
            }
            Check::Serial | Check::BlarggMemory if gb.cycles() >= next_poll => {
                next_poll = gb.cycles() + CYCLES_PER_SECOND / 10;
                if let Some(result) = blargg_result(case.check, &gb) {
                    return result;
                }
            }
            _ => {}
        }
    }
    if let Check::Screenshot(_) = case.check {
        return finish(case.check, &gb, &rom_path);
    }
    Err(format!(
        "timed out after {} emulated seconds",
        case.timeout_seconds
    ))
}

fn blargg_result(check: Check, gb: &Gb) -> Option<Result<(), String>> {
    if let Check::Serial = check {
        let text = String::from_utf8_lossy(gb.serial_output());
        if text.contains("Passed") {
            return Some(Ok(()));
        }
        if text.contains("Failed") {
            return Some(Err(text.trim().replace('\n', " ")));
        }
        return None;
    }
    let ram = gb.cartridge_ram();
    if ram.get(1..4) != Some(&[0xDE, 0xB0, 0x61][..]) || ram[0] == 0x80 {
        return None;
    }
    if ram[0] == 0 {
        return Some(Ok(()));
    }
    let message: String = ram[4..]
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| char::from(*byte))
        .collect();
    Some(Err(format!(
        "code {}: {}",
        ram[0],
        message.trim().replace('\n', " ")
    )))
}

fn finish(check: Check, gb: &Gb, rom_path: &Path) -> Result<(), String> {
    match check {
        Check::Screenshot(png) => {
            let expected = rom_path.with_file_name(png);
            let result = compare_screen(gb, &expected);
            if result.is_err() {
                // Keep what we drew next to the report, for comparing by eye.
                let actual = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/gb-test-roms")
                    .join(png.replace(".png", "-actual.png"));
                write_screen(gb, &actual).ok();
            }
            result
        }
        Check::Gambatte => compare_digits(gb, rom_path),
        Check::Microtest => match gb.peek(0xFF82) {
            0x01 => Ok(()),
            result => Err(format!("0xFF82 = {result:#04x}")),
        },
        _ => {
            let r = gb.registers();
            let got = [r.b, r.c, r.d, r.e, r.h, r.l];
            if got == [3, 5, 8, 13, 21, 34] {
                Ok(())
            } else {
                Err(format!("registers {got:?}"))
            }
        }
    }
}

/// Whether the screen's top row of tiles shows the hex digits after the
/// last `_out` in `rom_path`'s name, black on white in Gambatte's font.
fn compare_digits(gb: &Gb, rom_path: &Path) -> Result<(), String> {
    let stem = rom_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let digits: Vec<u32> = stem
        .rsplit_once("_out")
        .map(|(_, out)| out.chars().map_while(|c| c.to_digit(16)).collect())
        .unwrap_or_default();
    if digits.is_empty() {
        return Err("no expected digits in the file name".to_owned());
    }
    let screen = &gb.screens()[0];
    let width = usize::from(screen.size().width);
    for (index, digit) in digits.iter().enumerate() {
        let glyph = GAMBATTE_DIGITS[*digit as usize];
        for (y, row) in glyph.iter().enumerate() {
            for x in 0..8 {
                // Gambatte ignores the low three bits of each channel.
                let pixel = screen.pixels()[y * width + index * 8 + x] & 0xF8_F8F8;
                let black = row & (0x80 >> x) != 0;
                if pixel != if black { 0 } else { 0xF8_F8F8 } {
                    return Err(format!("digit {index} does not show {digit:X}"));
                }
            }
        }
    }
    Ok(())
}

fn write_screen(gb: &Gb, path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().expect("has a parent"))?;
    let screen = &gb.screens()[0];
    let size = screen.size();
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(fs::File::create(path)?),
        u32::from(size.width),
        u32::from(size.height),
    );
    encoder.set_color(png::ColorType::Rgb);
    let rgb: Vec<u8> = screen
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

fn compare_screen(gb: &Gb, png_path: &Path) -> Result<(), String> {
    let file = fs::File::open(png_path)
        .map_err(|error| format!("cannot open {}: {error}", png_path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("png too large")?];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| error.to_string())?;
    let channels = info.color_type.samples();
    let screen = &gb.screens()[0];
    let width = usize::from(screen.size().width);
    let mismatches: Vec<(usize, [u8; 3], [u8; 3])> = screen
        .pixels()
        .iter()
        .zip(buffer.chunks_exact(channels))
        .enumerate()
        .filter_map(|(index, (ours, theirs))| {
            let [b, g, r, _] = ours.to_le_bytes();
            // Greyscale PNGs have one or two channels.
            let expected = if channels < 3 {
                [theirs[0]; 3]
            } else {
                [theirs[0], theirs[1], theirs[2]]
            };
            ([r, g, b] != expected).then_some((index, [r, g, b], expected))
        })
        .collect();
    let Some(&(index, ours, expected)) = mismatches.first() else {
        return Ok(());
    };
    let hex = |[r, g, b]: [u8; 3]| format!("#{r:02x}{g:02x}{b:02x}");
    Err(format!(
        "{} pixels differ from {}; first at ({}, {}): ours {}, expected {}",
        mismatches.len(),
        png_path.display(),
        index % width,
        index / width,
        hex(ours),
        hex(expected),
    ))
}

#[test]
fn test_roms() {
    if !bundle_root().exists() {
        eprintln!("skipping: run scripts/fetch-test-roms.sh to download the test ROMs");
        return;
    }
    let cases = cases();
    let results = Mutex::new(Vec::with_capacity(cases.len()));
    let next = Mutex::new(cases.iter());
    let workers = thread::available_parallelism().map_or(4, usize::from);
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some(case) = next.lock().expect("lock").next() else {
                        break;
                    };
                    let result = run(case);
                    results
                        .lock()
                        .expect("lock")
                        .push((case.label.clone(), result));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("lock");
    results.sort_by(|a, b| a.0.cmp(&b.0));

    let mut report = String::new();
    for (path, result) in &results {
        match result {
            Ok(()) => writeln!(report, "PASS {path}"),
            Err(why) => writeln!(report, "FAIL {path}: {why}"),
        }
        .expect("writing to a String");
    }
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/gb-test-roms.txt");
    fs::write(&target, &report).expect("writing the report");

    let passing: Vec<&str> = results
        .iter()
        .filter(|(_, result)| result.is_ok())
        .map(|(path, _)| path.as_str())
        .collect();
    let ratchet_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test-roms-passing.txt");
    if std::env::var_os("UPDATE_PASSING").is_some() {
        fs::write(&ratchet_path, passing.join("\n") + "\n").expect("writing the ratchet");
    }
    let expected = fs::read_to_string(&ratchet_path).unwrap_or_default();
    let regressions: Vec<&str> = expected
        .lines()
        .filter(|line| !line.is_empty() && !passing.contains(line))
        .collect();
    let newly_passing: Vec<&str> = passing
        .iter()
        .copied()
        .filter(|path| !expected.lines().any(|line| line == *path))
        .collect();

    println!("{report}");
    println!("{} of {} test ROMs pass", passing.len(), results.len());
    if !newly_passing.is_empty() {
        println!(
            "newly passing (record with UPDATE_PASSING=1):\n  {}",
            newly_passing.join("\n  ")
        );
    }
    assert!(
        regressions.is_empty(),
        "regressions:\n  {}",
        regressions.join("\n  ")
    );
}
