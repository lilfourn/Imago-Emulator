//! Runs open-source DS test ROMs against the core. Fetch them with
//! `scripts/fetch-test-roms.sh`; a suite whose ROMs are missing prints a
//! note and passes.
//!
//! - RockPolish's rockwrestler runs one test per menu entry, then writes
//!   "OK", or "FAIL" or "TIMEOUT" and the failing check's number, at the
//!   top left of the top screen. The harness opens each test from power-on
//!   and reads that line with rockwrestler's own font. It also checks the
//!   light at the top right, which is green while the ARM7 answers.
//! - armwrestler (Atem2069's fixed build of the DS version) shows one page
//!   of results per menu entry, as on the GBA: an instruction name, then
//!   "OK" in green, or "BAD" and what was wrong in red. The harness presses
//!   its way to each page, checks its title, and reads the lines with
//!   armwrestler's font. A page passes if it has results and none is BAD.
//!
//! `rockwrestler-passing.txt` and `armwrestler-passing.txt` list what must
//! keep passing; record improvements with `UPDATE_PASSING=1`. Reports go
//! to `target/nds-*.txt`, and the screens of failing cases to
//! `target/nds-test-roms/`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use emu_core::{Button, Core, Frame, Input, Media};
use emu_nds::Nds;

/// Frames each scripted press is held, and released after it.
const PRESS_FRAMES: u32 = 10;
/// Frames a test ROM gets to start before the first press.
const BOOT_FRAMES: u32 = 60;

fn test_roms() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-roms")
}

fn target(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(name)
}

fn power_on(path: &Path) -> Result<Nds, String> {
    let rom = fs::read(path).map_err(|error| format!("cannot read: {error}"))?;
    Nds::new(&Media::new(&rom)).map_err(|error| error.to_string())
}

fn run_frames(nds: &mut Nds, input: Input, frames: u32) {
    for _ in 0..frames {
        nds.run_frame(&[input], &mut Vec::new());
    }
}

/// Taps each button in turn: held for a few frames, then released.
fn press(nds: &mut Nds, buttons: &[Button]) {
    for &button in buttons {
        let held = Input {
            buttons: [button].into_iter().collect(),
            ..Input::default()
        };
        run_frames(nds, held, PRESS_FRAMES);
        run_frames(nds, Input::default(), PRESS_FRAMES);
    }
}

/// Saves both screens, top above bottom, as
/// `target/nds-test-roms/NAME.png`.
fn keep_screens(nds: &Nds, name: &str) {
    let file = name.replace(['/', ' ', '.', ',', '{', '}'], "-") + ".png";
    let path = target("nds-test-roms").join(file);
    let write = || -> std::io::Result<()> {
        fs::create_dir_all(path.parent().expect("has a parent"))?;
        let width = nds.screens()[0].size().width;
        let height: u16 = nds
            .screens()
            .iter()
            .map(|screen| screen.size().height)
            .sum();
        let mut encoder = png::Encoder::new(
            std::io::BufWriter::new(fs::File::create(&path)?),
            u32::from(width),
            u32::from(height),
        );
        encoder.set_color(png::ColorType::Rgb);
        let rgb: Vec<u8> = nds
            .screens()
            .iter()
            .flat_map(Frame::pixels)
            .flat_map(|pixel| {
                let [b, g, r, _] = pixel.to_le_bytes();
                [r, g, b]
            })
            .collect();
        encoder.write_header()?.write_image_data(&rgb)?;
        Ok(())
    };
    write().ok();
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

/// Writes the report `name`, then checks `passing` against the ratchet
/// `name` next to this test, rewriting the ratchet first under
/// `UPDATE_PASSING`. Fails the test if a listed case no longer passes.
fn finish(name: &str, results: &[(String, Result<String, String>)]) {
    let mut report = String::new();
    let mut passing = Vec::new();
    for (label, result) in results {
        match result {
            Ok(how) => {
                passing.push(label.as_str());
                writeln!(report, "PASS {label}: {how}")
            }
            Err(why) => writeln!(report, "FAIL {label}: {why}"),
        }
        .expect("writing to a String");
    }
    fs::write(target(&format!("nds-{name}.txt")), &report).expect("writing the report");
    println!("{report}");

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(format!("{name}-passing.txt"));
    if std::env::var_os("UPDATE_PASSING").is_some() {
        fs::write(&path, passing.join("\n") + "\n").expect("writing the ratchet");
    }
    let expected = fs::read_to_string(&path).unwrap_or_default();
    let regressions: Vec<&str> = expected
        .lines()
        .filter(|line| !line.is_empty() && !passing.contains(line))
        .collect();
    let newly_passing: Vec<&&str> = passing
        .iter()
        .filter(|label| !expected.lines().any(|line| line == **label))
        .collect();
    if !newly_passing.is_empty() {
        println!("newly passing (record with UPDATE_PASSING=1): {newly_passing:?}");
    }
    assert!(regressions.is_empty(), "regressions: {regressions:?}");
}

/// An 8×8 font, one byte per pixel: `first` is the character of the first
/// glyph, and `ink` tells which bytes are drawn.
struct Font {
    glyphs: Vec<u8>,
    first: u8,
    ink: fn(u8) -> bool,
}

impl Font {
    /// The character in the 8×8 cell at (`x`, `y`), where `ink` tells which
    /// pixels are drawn: a space if none is, `?` if no glyph matches.
    fn read_cell(&self, screen: &Frame, x: usize, y: usize, ink: impl Fn(u32) -> bool) -> char {
        let width = usize::from(screen.size().width);
        let cell: Vec<bool> = (0..64)
            .map(|i| ink(screen.pixels()[(y + i / 8) * width + x + i % 8]))
            .collect();
        if !cell.contains(&true) {
            return ' ';
        }
        self.glyphs
            .chunks_exact(64)
            .position(|glyph| {
                glyph
                    .iter()
                    .zip(&cell)
                    .all(|(byte, drawn)| (self.ink)(*byte) == *drawn)
            })
            .and_then(|index| u8::try_from(index).ok())
            .and_then(|index| index.checked_add(self.first))
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
}

/// Whether `pixel` (0xAARRGGBB) has only red, only green, or no colour.
fn only_red(pixel: u32) -> bool {
    pixel & 0x0000_FFFF == 0 && pixel & 0x00FF_0000 != 0
}

fn only_green(pixel: u32) -> bool {
    pixel & 0x00FF_00FF == 0 && pixel & 0x0000_FF00 != 0
}

fn black(pixel: u32) -> bool {
    pixel & 0x00FF_FFFF == 0
}

/// rockwrestler's tests, by main-menu entry, the title of the submenu it
/// opens, and the tests listed there. The last entry, INITIAL STATE, only
/// prints values, and is left out.
const ROCKWRESTLER_MENUS: [(&str, &str, &[&str]); 5] = [
    ("ARMv4", "ARMv4", &["CONDITION CODES"]),
    (
        "ARMv5",
        "ARMv5",
        &[
            "CLZ",
            "QADD, QSUB",
            "QDADD, QDSUB",
            "SMULxy",
            "SMLAxy",
            "SMULWy",
            "SMLAWy",
            "SMLALxy",
            "BLX",
            "LDR r15, POP {r15}, LDM {r15}",
            "LDM / STM",
        ],
    ),
    ("IPC", "IPC TESTS", &["IPCSYNC", "IPCFIFO", "IPCFIFO IRQ"]),
    (
        "DS MATH",
        "DS MATH",
        &["SQRT 32", "SQRT 64", "DIV 32/32", "DIV 64/32", "DIV 64/64"],
    ),
    ("MEMORY", "MEMORY CONTROL", &["WRAM CNT", "VRAM CNT", "TCM"]),
];

/// Emulated seconds a rockwrestler test may take to report.
const ROCKWRESTLER_SECONDS: u32 = 10;

/// rockwrestler's font: glyphs for every byte, with ASCII from 0x20 on.
/// Text is black on white.
fn rockwrestler_font(glyphs: &[u8]) -> Font {
    Font {
        glyphs: glyphs.get(0x20 * 64..).unwrap_or_default().to_vec(),
        first: 0x20,
        ink: |byte| byte == 0,
    }
}

/// The text in row `row` of rockwrestler's 32×24 grid on the top screen.
fn rockwrestler_row(nds: &Nds, font: &Font, row: usize) -> String {
    font.read_line(&nds.screens()[0], 0, row * 8, black)
}

/// Whether rockwrestler's ARM7 light, the top-right cell, is green.
fn arm7_light_green(nds: &Nds) -> bool {
    let screen = &nds.screens()[0];
    let width = usize::from(screen.size().width);
    (0..8).all(|y| {
        screen.pixels()[y * width + width - 8..(y + 1) * width]
            .iter()
            .all(|pixel| only_green(*pixel))
    })
}

/// Runs rockwrestler test `test` of main-menu entry `entry` from power-on;
/// `Ok` if it reports OK.
fn run_rockwrestler(rom: &Path, font: &Font, entry: usize, test: usize) -> Result<String, String> {
    let (_, title, tests) = ROCKWRESTLER_MENUS[entry];
    let mut nds = power_on(rom)?;
    run_frames(&mut nds, Input::default(), BOOT_FRAMES);
    press(&mut nds, &vec![Button::Down; entry]);
    press(&mut nds, &[Button::A]);
    press(&mut nds, &vec![Button::Down; test]);
    let opened = rockwrestler_row(&nds, font, 0);
    let listed = font.read_line(&nds.screens()[0], 16, (2 + test) * 8, black);
    let cursor = font.read_cell(&nds.screens()[0], 8, (2 + test) * 8, black);
    if !opened.starts_with(title) || listed != tests[test] || cursor == ' ' {
        keep_screens(&nds, &format!("rockwrestler {title} {}", tests[test]));
        return Err(format!("menu shows {opened:?} at {listed:?}"));
    }
    let held = Input {
        buttons: [Button::A].into_iter().collect(),
        ..Input::default()
    };
    for frame in 0..ROCKWRESTLER_SECONDS * 60 {
        let input = if frame < PRESS_FRAMES {
            held
        } else {
            Input::default()
        };
        run_frames(&mut nds, input, 1);
        let result = rockwrestler_row(&nds, font, 0);
        if result.starts_with("OK") {
            return Ok("OK".to_owned());
        }
        if result.starts_with("FAIL") || result.starts_with("TIMEOUT") {
            keep_screens(&nds, &format!("rockwrestler {title} {}", tests[test]));
            return Err(result);
        }
    }
    keep_screens(&nds, &format!("rockwrestler {title} {}", tests[test]));
    Err(format!(
        "no result after {ROCKWRESTLER_SECONDS} emulated seconds"
    ))
}

#[test]
fn rockwrestler() {
    let bundle = test_roms().join("rockwrestler");
    let rom = bundle.join("rockwrestler.nds");
    let Ok(glyphs) = fs::read(bundle.join("src9/framework/font.bin")) else {
        eprintln!("skipping: run scripts/fetch-test-roms.sh to download rockwrestler");
        return;
    };
    let font = rockwrestler_font(&glyphs);
    let mut results = Vec::new();
    // The ARM7 answers through IPCSYNC from boot; the menu shows whether it
    // does.
    let light = power_on(&rom).map(|mut nds| {
        run_frames(&mut nds, Input::default(), BOOT_FRAMES);
        arm7_light_green(&nds)
    });
    results.push((
        "ARM7 light".to_owned(),
        match light {
            Ok(true) => Ok("green".to_owned()),
            Ok(false) => Err("red".to_owned()),
            Err(why) => Err(why),
        },
    ));
    let cases: Vec<(usize, usize)> = ROCKWRESTLER_MENUS
        .iter()
        .enumerate()
        .flat_map(|(entry, (_, _, tests))| (0..tests.len()).map(move |test| (entry, test)))
        .collect();
    let outcomes = in_parallel(&cases, |&(entry, test)| {
        run_rockwrestler(&rom, &font, entry, test)
    });
    for (&(entry, test), outcome) in cases.iter().zip(outcomes) {
        let (name, _, tests) = ROCKWRESTLER_MENUS[entry];
        results.push((format!("{name}/{}", tests[test]), outcome));
    }
    finish("rockwrestler", &results);
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

/// The DS build's THUMB LDM/STM page has a title but no tests, so it is
/// left out.
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
            label: "ARM V5TE",
            title: "ARM V5TE TESTS",
            presses: &[Down, Down, Down, Down, Down, Down, Start],
        },
    ]
};

/// armwrestler's font: glyphs from `%` (0x25) on, non-zero bytes drawn.
fn armwrestler_font(glyphs: Vec<u8>) -> Font {
    Font {
        glyphs,
        first: 0x25,
        ink: |byte| byte != 0,
    }
}

/// The title at the top of an armwrestler page. Titles start at any
/// column, so this reads from each of the eight offsets and keeps the one
/// that makes sense.
fn armwrestler_title(screen: &Frame, font: &Font) -> String {
    (0..8)
        .map(|x| font.read_line(screen, x, 1, |pixel| !black(pixel)))
        .filter(|title| !title.contains('?'))
        .max_by_key(String::len)
        .unwrap_or_default()
}

/// Reads an armwrestler results page: each 8-pixel text line from y = 16
/// with green ink is a pass, with red ink a failure. Returns the number of
/// results, or the failing lines' text.
fn read_armwrestler_page(screen: &Frame, font: &Font) -> Result<usize, String> {
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
            bad.push(font.read_line(screen, 0, y, |pixel| !black(pixel)));
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

fn run_armwrestler(rom: &Path, font: &Font, page: &Page) -> Result<String, String> {
    let mut nds = power_on(rom)?;
    run_frames(&mut nds, Input::default(), BOOT_FRAMES);
    press(&mut nds, page.presses);
    run_frames(&mut nds, Input::default(), 10);
    let screen = &nds.screens()[0];
    let title = armwrestler_title(screen, font);
    let result = if title == page.title {
        read_armwrestler_page(screen, font).map(|count| format!("{count} OK"))
    } else {
        Err(format!("opened {title:?}, not {:?}", page.title))
    };
    if result.is_err() {
        keep_screens(&nds, &format!("armwrestler {}", page.label));
    }
    result
}

#[test]
fn armwrestler() {
    let bundle = test_roms().join("armwrestler-ds");
    let rom = bundle.join("armwrestler.nds");
    let Ok(glyphs) = fs::read(bundle.join("font8x8.pat")) else {
        eprintln!("skipping: run scripts/fetch-test-roms.sh to download armwrestler");
        return;
    };
    let font = armwrestler_font(glyphs);
    let outcomes = in_parallel(&ARMWRESTLER_PAGES, |page| {
        run_armwrestler(&rom, &font, page)
    });
    let results: Vec<(String, Result<String, String>)> = ARMWRESTLER_PAGES
        .iter()
        .map(|page| page.label.to_owned())
        .zip(outcomes)
        .collect();
    finish("armwrestler", &results);
}
