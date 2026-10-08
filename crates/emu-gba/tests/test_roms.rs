//! Runs open-source GBA test ROMs against the core. Fetch jsmolka's tests
//! and armwrestler with `scripts/fetch-test-roms.sh`, and the mGBA suite
//! with `scripts/fetch-mgba-suite.sh`; a suite whose ROMs are missing
//! prints a note and passes.
//!
//! - jsmolka's ROMs run their tests, leave the number of the first failure
//!   in R12 (0 when all pass), and park in an infinite `B .` loop. A ROM
//!   passes if it parks with R12 = 0.
//! - armwrestler shows one page of results per menu entry: an instruction
//!   name, then "OK" in green, or "BAD" and what was wrong in red. The
//!   harness presses its way to each page from power-on and reads the
//!   lines off the screen with armwrestler's own font. A page passes if it
//!   has results and none is BAD.
//! - The mGBA suite runs one group of tests per menu entry, then prints
//!   `passes/total` at the top of the screen. The harness opens each group
//!   from power-on and reads that text from the background map in VRAM.
//!
//! Each suite has a ratchet file next to this one: `test-roms-passing.txt`
//! and `armwrestler-passing.txt` list what must keep passing, and
//! `mgba-suite-passing.txt` how many tests in each group must. Record
//! improvements with `UPDATE_PASSING=1`. Reports go to `target/gba-*.txt`,
//! the screens of failing cases to `target/gba-test-roms/`, and the
//! failure log the mGBA suite keeps in SRAM to `target/gba-mgba-suite/`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use emu_core::{Button, Core, Frame, Input, Media, MemoryRegion};
use emu_gba::Gba;

const CYCLES_PER_SECOND: u64 = 16_777_216;
/// Steps with an unchanged PC that mean the ROM is parked.
const PARKED_STEPS: u32 = 64;
/// Frames each scripted press is held, and released after it. Menus that
/// redraw from ROM can take a few frames per pass and miss shorter taps.
const PRESS_FRAMES: u32 = 10;
/// Frames a menu-driven ROM gets to start before the first press.
const BOOT_FRAMES: u32 = 30;

fn test_roms() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-roms")
}

fn target(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(name)
}

/// jsmolka's ROMs that report through R12, relative to `gba-tests`.
const JSMOLKA: [&str; 9] = [
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

fn power_on(path: &Path) -> Result<Gba, String> {
    let rom = fs::read(path).map_err(|error| format!("cannot read: {error}"))?;
    Gba::new(&Media::new(&rom)).map_err(|error| error.to_string())
}

fn run_jsmolka(path: &str) -> Result<(), String> {
    let mut gba = power_on(&test_roms().join("gba-tests").join(path))?;
    let mut last_pc = 0;
    let mut parked = 0;
    while gba.cycles() < 10 * CYCLES_PER_SECOND {
        gba.step();
        let pc = gba.cpu().reg(15);
        parked = if pc == last_pc { parked + 1 } else { 0 };
        last_pc = pc;
        if parked >= PARKED_STEPS {
            // Let the result reach the screen before judging.
            run_frames(&mut gba, Input::default(), 2);
            return match gba.cpu().reg(12) {
                0 => Ok(()),
                failed => Err(format!("test {failed} failed")),
            };
        }
    }
    Err(format!("never parked; PC = {:08x}", gba.cpu().reg(15)))
}

fn write_screen(gba_screen: &Frame, path: &Path) -> std::io::Result<()> {
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

/// `label` as a file name.
fn file_name(label: &str, extension: &str) -> String {
    label.replace(['/', ' ', '.'], "-") + extension
}

/// Saves the screen of a failing case as `target/gba-test-roms/NAME.png`.
fn keep_screen(gba: &Gba, name: &str) {
    let path = target("gba-test-roms").join(file_name(name, ".png"));
    write_screen(&gba.screens()[0], &path).ok();
}

/// Runs `path` for a few seconds and saves its screen, for ROMs judged by
/// eye.
fn snapshot(path: &str) {
    let Ok(mut gba) = power_on(&test_roms().join("gba-tests").join(path)) else {
        return;
    };
    run_frames(&mut gba, Input::default(), 120);
    keep_screen(&gba, path.trim_end_matches(".gba"));
}

fn run_frames(gba: &mut Gba, input: Input, frames: u32) {
    for _ in 0..frames {
        gba.run_frame(&[input], &mut Vec::new());
    }
}

/// Taps each button in turn: held for a few frames, then released.
fn press(gba: &mut Gba, buttons: &[Button]) {
    for &button in buttons {
        let held = Input {
            buttons: [button].into_iter().collect(),
            ..Input::default()
        };
        run_frames(gba, held, PRESS_FRAMES);
        run_frames(gba, Input::default(), PRESS_FRAMES);
    }
}

/// Runs `job` on every item across all cores, keeping the items' order.
fn in_parallel<T: Sync, R: Send>(items: &[T], job: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = Mutex::new(items.iter().enumerate());
    let results = Mutex::new(Vec::with_capacity(items.len()));
    let workers = thread::available_parallelism().map_or(4, usize::from);
    thread::scope(|scope| {
        for _ in 0..workers.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let Some((index, item)) = next.lock().expect("lock").next() else {
                        break;
                    };
                    let result = job(item);
                    results.lock().expect("lock").push((index, result));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("lock");
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

/// Checks `passing` against the ratchet file `name` next to this test,
/// rewriting the file first under `UPDATE_PASSING`. Fails the test if a
/// listed case no longer passes.
fn check_ratchet(name: &str, passing: &[String]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(name);
    if std::env::var_os("UPDATE_PASSING").is_some() {
        fs::write(&path, passing.join("\n") + "\n").expect("writing the ratchet");
    }
    let expected = fs::read_to_string(&path).unwrap_or_default();
    let regressions: Vec<&str> = expected
        .lines()
        .filter(|line| !line.is_empty() && !passing.iter().any(|label| label == line))
        .collect();
    let newly_passing: Vec<&String> = passing
        .iter()
        .filter(|label| !expected.lines().any(|line| line == label.as_str()))
        .collect();
    if !newly_passing.is_empty() {
        println!("newly passing (record with UPDATE_PASSING=1): {newly_passing:?}");
    }
    assert!(regressions.is_empty(), "regressions: {regressions:?}");
}

#[test]
fn jsmolka() {
    if !test_roms().join("gba-tests").exists() {
        eprintln!("skipping: run scripts/fetch-test-roms.sh to download jsmolka's tests");
        return;
    }
    let mut report = String::new();
    let mut passing = Vec::new();
    for path in JSMOLKA {
        match run_jsmolka(path) {
            Ok(()) => {
                passing.push(path.to_owned());
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
    check_ratchet("test-roms-passing.txt", &passing);
}

/// One armwrestler results page.
struct Page {
    /// Name in the report and the ratchet.
    label: &'static str,
    /// The title armwrestler draws at the top of the page.
    title: &'static str,
    /// Presses from power-on that show the page: Down to its menu entry,
    /// Start to open it, and Start again for an entry's second page.
    presses: &'static [Button],
}

const ARMWRESTLER_PAGES: [Page; 8] = {
    use Button::{Down, Start};
    [
        Page {
            label: "ARM ALU 1",
            title: "ALU TESTS PART 1",
            presses: &[Start],
        },
        Page {
            label: "ARM ALU 2",
            title: "ALU PT 2 / MISC",
            presses: &[Start, Start],
        },
        Page {
            label: "ARM LDR/STR 1",
            title: "LOAD TESTS PART 1",
            presses: &[Down, Start],
        },
        Page {
            label: "ARM LDR/STR 2",
            title: "LOAD TESTS PART 2",
            presses: &[Down, Start, Start],
        },
        Page {
            label: "ARM LDM/STM",
            title: "LDM/STM TESTS 1",
            presses: &[Down, Down, Start],
        },
        Page {
            label: "THUMB ALU",
            title: "ALU TEST",
            presses: &[Down, Down, Down, Start],
        },
        Page {
            label: "THUMB LDR/STR",
            title: "LDR/STR TEST 1",
            presses: &[Down, Down, Down, Down, Start],
        },
        Page {
            label: "THUMB LDM/STM",
            title: "LDM/STM TEST",
            presses: &[Down, Down, Down, Down, Down, Start],
        },
    ]
};

/// armwrestler's 8×8 font: one byte per pixel, non-zero for ink, glyphs
/// from `%` (0x25) on.
struct Font(Vec<u8>);

impl Font {
    /// The character drawn in the 8×8 cell at (`x`, `y`), where `ink` tells
    /// which pixels are drawn; `?` if no glyph matches.
    fn read_cell(&self, screen: &Frame, x: usize, y: usize, ink: impl Fn(u32) -> bool) -> char {
        let width = usize::from(screen.size().width);
        let cell: Vec<bool> = (0..64)
            .map(|i| ink(screen.pixels()[(y + i / 8) * width + x + i % 8]))
            .collect();
        if !cell.contains(&true) {
            return ' ';
        }
        self.0
            .chunks_exact(64)
            .position(|glyph| glyph.iter().zip(&cell).all(|(g, c)| (*g != 0) == *c))
            .and_then(|index| u8::try_from(index + 0x25).ok())
            .map_or('?', char::from)
    }

    /// The text in the row of 8×8 cells whose top left is (`x`, `y`), with
    /// runs of spaces collapsed.
    fn read_line(&self, screen: &Frame, x: usize, y: usize, ink: impl Fn(u32) -> bool) -> String {
        let width = usize::from(screen.size().width);
        let text: String = (x..=width - 8)
            .step_by(8)
            .map(|x| self.read_cell(screen, x, y, &ink))
            .collect();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The title at the top of an armwrestler page. Titles start at any
    /// column, so this reads from each of the eight offsets and keeps the
    /// one that makes sense.
    fn read_title(&self, screen: &Frame) -> String {
        (0..8)
            .map(|x| self.read_line(screen, x, 1, |pixel| pixel & 0x00FF_FFFF != 0))
            .filter(|title| !title.contains('?'))
            .max_by_key(String::len)
            .unwrap_or_default()
    }
}

/// Whether `pixel` (0xAARRGGBB) has only red, or only green.
fn only_red(pixel: u32) -> bool {
    pixel & 0x0000_FFFF == 0 && pixel & 0x00FF_0000 != 0
}

fn only_green(pixel: u32) -> bool {
    pixel & 0x00FF_00FF == 0 && pixel & 0x0000_FF00 != 0
}

/// Reads an armwrestler results page: each 8-pixel text line from y = 16
/// with green ink is a pass, with red ink a failure. Returns the number of
/// results, or the failing lines' text.
fn read_wrestler_page(screen: &Frame, font: &Font) -> Result<usize, String> {
    let width = usize::from(screen.size().width);
    let height = usize::from(screen.size().height);
    let mut results = 0;
    let mut bad = Vec::new();
    for y in (16..=height - 8).step_by(8) {
        let band = &screen.pixels()[y * width..(y + 8) * width];
        let red = band.iter().any(|pixel| only_red(*pixel));
        if !red && !band.iter().any(|pixel| only_green(*pixel)) {
            continue;
        }
        results += 1;
        if red {
            bad.push(font.read_line(screen, 0, y, |pixel| pixel & 0x00FF_FFFF != 0));
        }
    }
    match (results, bad.is_empty()) {
        (0, _) => Err("no results on screen".to_owned()),
        (_, true) => Ok(results),
        _ => Err(format!(
            "{} of {results} BAD: {}",
            bad.len(),
            bad.join("; ")
        )),
    }
}

fn run_armwrestler(rom: &Path, font: &Font, page: &Page) -> Result<usize, String> {
    let mut gba = power_on(rom)?;
    run_frames(&mut gba, Input::default(), BOOT_FRAMES);
    press(&mut gba, page.presses);
    run_frames(&mut gba, Input::default(), 10);
    let screen = &gba.screens()[0];
    let title = font.read_title(screen);
    let result = if title == page.title {
        read_wrestler_page(screen, font)
    } else {
        Err(format!("opened {title:?}, not {:?}", page.title))
    };
    if result.is_err() {
        keep_screen(&gba, &format!("armwrestler {}", page.label));
    }
    result
}

#[test]
fn armwrestler() {
    let bundle = test_roms().join("armwrestler-gba");
    let rom = bundle.join("armwrestler-gba-fixed.gba");
    let Ok(font) = fs::read(bundle.join("data/font8x8.pat")) else {
        eprintln!("skipping: run scripts/fetch-test-roms.sh to download armwrestler");
        return;
    };
    let font = Font(font);
    let results = in_parallel(&ARMWRESTLER_PAGES, |page| {
        run_armwrestler(&rom, &font, page)
    });
    let mut report = String::new();
    let mut passing = Vec::new();
    for (Page { label, .. }, result) in ARMWRESTLER_PAGES.iter().zip(results) {
        match result {
            Ok(count) => {
                passing.push((*label).to_owned());
                writeln!(report, "PASS {label}: {count} OK")
            }
            Err(why) => writeln!(report, "FAIL {label}: {why}"),
        }
        .expect("writing to a String");
    }
    fs::write(target("gba-armwrestler.txt"), &report).expect("writing the report");
    println!("{report}");
    check_ratchet("armwrestler-passing.txt", &passing);
}

/// The mGBA suite's test groups, in menu order. Its last entry, the video
/// tests, has no automatic checks and is left out.
const MGBA_GROUPS: [&str; 13] = [
    "Memory tests",
    "I/O read tests",
    "Timing tests",
    "Timer count-up tests",
    "Timer IRQ tests",
    "Shifter tests",
    "Carry tests",
    "Multiply long tests",
    "BIOS math tests",
    "DMA tests",
    "SIO register R/W tests",
    "SIO timing tests",
    "Misc. edge case tests",
];

/// Emulated seconds a group may take to finish.
const MGBA_GROUP_SECONDS: u32 = 60;

/// Row `row` of the mGBA suite's text: screen block 1 of VRAM holds one
/// tile per character, numbered from the space.
fn mgba_text_row(gba: &Gba, row: usize) -> String {
    let vram = gba
        .memory(MemoryRegion::VideoRam)
        .expect("the GBA exposes VRAM");
    vram[0x800 + row * 64..0x800 + row * 64 + 60]
        .chunks_exact(2)
        .map(|entry| {
            let tile = u16::from_le_bytes([entry[0], entry[1]]) & 0x3FF;
            u8::try_from(tile)
                .ok()
                .and_then(|tile| tile.checked_add(b' '))
                .filter(u8::is_ascii_graphic)
                .map_or(' ', char::from)
        })
        .collect()
}

/// The text the suite logs to SRAM: a header, then one line per failure.
fn mgba_log(gba: &Gba) -> String {
    let mut sram = Vec::new();
    gba.save_backup(0, &mut sram);
    let end = sram
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(sram.len());
    String::from_utf8_lossy(&sram[..end]).into_owned()
}

/// Opens group `index` from power-on and waits for its `passes/total`.
fn run_mgba_group(rom: &Path, index: usize, name: &str) -> Result<(u32, u32), String> {
    let mut gba = power_on(rom)?;
    run_frames(&mut gba, Input::default(), BOOT_FRAMES);
    press(&mut gba, &vec![Button::Down; index]);
    press(&mut gba, &[Button::A]);
    for _ in 0..MGBA_GROUP_SECONDS * 60 {
        run_frames(&mut gba, Input::default(), 1);
        let header = mgba_text_row(&gba, 1);
        let Some((passes, total)) = header.get(21..).and_then(|counts| counts.split_once('/'))
        else {
            continue;
        };
        // The counts overwrite the end of long names.
        if !header.starts_with(name.get(..21).unwrap_or(name)) {
            return Err(format!("opened {:?}", header.trim()));
        }
        fs::create_dir_all(target("gba-mgba-suite")).ok();
        let log = target("gba-mgba-suite").join(file_name(name, ".log"));
        fs::write(log, mgba_log(&gba)).ok();
        let count = |text: &str| {
            text.trim()
                .parse::<u32>()
                .map_err(|error| error.to_string())
        };
        return Ok((count(passes)?, count(total)?));
    }
    keep_screen(&gba, &format!("mgba-suite {name}"));
    Err(format!(
        "no result after {MGBA_GROUP_SECONDS} emulated seconds"
    ))
}

/// The `name: passes/total` lines of the mGBA ratchet, as (name, passes).
fn mgba_ratchet(text: &str) -> Vec<(&str, u32)> {
    text.lines()
        .filter_map(|line| {
            let (name, counts) = line.rsplit_once(": ")?;
            let (passes, _) = counts.split_once('/')?;
            Some((name, passes.parse().ok()?))
        })
        .collect()
}

#[test]
fn mgba_suite() {
    let rom = test_roms().join("mgba-suite/suite.gba");
    if !rom.exists() {
        eprintln!("skipping: run scripts/fetch-mgba-suite.sh to download the mGBA suite");
        return;
    }
    let groups: Vec<(usize, &str)> = MGBA_GROUPS.into_iter().enumerate().collect();
    let results = in_parallel(&groups, |(index, name)| run_mgba_group(&rom, *index, name));
    let mut report = String::new();
    let mut counts = String::new();
    for ((_, name), result) in groups.iter().zip(&results) {
        match result {
            Ok((passes, total)) => {
                writeln!(counts, "{name}: {passes}/{total}").expect("writing to a String");
                writeln!(report, "{name}: {passes}/{total}")
            }
            Err(why) => writeln!(report, "{name}: FAIL {why}"),
        }
        .expect("writing to a String");
    }
    fs::write(target("gba-mgba-suite.txt"), &report).expect("writing the report");
    println!("{report}");

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mgba-suite-passing.txt");
    if std::env::var_os("UPDATE_PASSING").is_some() {
        fs::write(&path, &counts).expect("writing the ratchet");
    }
    let expected = fs::read_to_string(&path).unwrap_or_default();
    let now = mgba_ratchet(&counts);
    let mut regressions = Vec::new();
    for (name, floor) in mgba_ratchet(&expected) {
        match now.iter().find(|(group, _)| *group == name) {
            Some((_, passes)) if *passes > floor => {
                println!("{name}: {passes} pass, up from {floor} (record with UPDATE_PASSING=1)");
            }
            Some((_, passes)) if *passes == floor => {}
            Some((_, passes)) => regressions.push(format!("{name}: {passes}, down from {floor}")),
            None => regressions.push(format!("{name}: no result")),
        }
    }
    assert!(regressions.is_empty(), "regressions: {regressions:?}");
}
