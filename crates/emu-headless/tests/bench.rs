//! The headless runner's benchmark options, `--state`, `--time` and
//! `--digest`, on a Game Boy ROM made here: a loop that counts in work RAM
//! and plays a tone, so frames, sound and state all move.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use emu_core::{CoreInfo, Input, Media, System};

/// A 32 KiB ROM that turns the sound on, starts a square wave on channel
/// 2, then counts in a loop: `INC (HL)` at 0xC000, forever.
fn counting_rom() -> Vec<u8> {
    let mut rom = vec![0; 0x8000];
    let code = [
        0x3E, 0x80, 0xE0, 0x26, // LD A,0x80; LDH (NR52),A: sound on
        0x3E, 0x77, 0xE0, 0x24, // LD A,0x77; LDH (NR50),A: full volume
        0x3E, 0xFF, 0xE0, 0x25, // LD A,0xFF; LDH (NR51),A: every channel both sides
        0x3E, 0xF0, 0xE0, 0x17, // LD A,0xF0; LDH (NR22),A: loud, no envelope
        0x3E, 0x87, 0xE0, 0x19, // LD A,0x87; LDH (NR24),A: trigger
        0x21, 0x00, 0xC0, // LD HL,0xC000
        0x34, // loop: INC (HL)
        0x18, 0xFD, // JR loop
    ];
    rom[0x100..0x100 + code.len()].copy_from_slice(&code);
    rom[0x14D] = rom[0x134..0x14D]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
    rom
}

/// A scratch folder, removed when the test ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

impl Scratch {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("emu-headless-{test}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("creating a scratch folder");
        Self(dir)
    }

    /// Writes `bytes` as `name` and returns its path as text.
    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        fs::write(&path, bytes).expect("writing a scratch file");
        path.to_str().expect("a UTF-8 path").to_owned()
    }
}

fn headless(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_emu-headless"))
        .args(args)
        .output()
        .expect("running emu-headless")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("UTF-8 output")
}

/// The output without its `elapsed_ms` line, which differs run to run.
fn without_time(text: &str) -> String {
    text.lines()
        .filter(|line| !line.starts_with("elapsed_ms="))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// A save state of `rom` after `frames` frames, in its container.
fn state_after(rom: &[u8], frames: u32) -> Vec<u8> {
    let mut core = emu_cores::create(System::Gb, &Media::new(rom)).expect("the ROM loads");
    let mut audio = Vec::new();
    for _ in 0..frames {
        core.run_frame(&[Input::default()], &mut audio);
    }
    let mut payload = Vec::new();
    core.save_state(&mut payload);
    let CoreInfo { state_version, .. } = core.info();
    let header = emu_state::Header {
        system: System::Gb,
        state_version,
        rom_crc32: crc32fast::hash(rom),
    };
    emu_state::encode(&header, &payload, None)
}

#[test]
fn the_digest_is_the_same_every_run_and_moves_with_the_game() {
    let scratch = Scratch::new("digest");
    let rom = scratch.write("count.gb", &counting_rom());
    let run = |frames: &str| stdout(&headless(&["--frames", frames, "--digest", &rom]));
    let first = run("20");
    assert_eq!(first, run("20"));

    let digest = |text: &str| text.lines().last().unwrap_or_default().to_owned();
    assert!(digest(&first).starts_with("audio_crc32="), "{first}");
    let later = run("21");
    let field = |line: &str, name: &str| {
        line.split(' ')
            .find(|field| field.starts_with(name))
            .unwrap_or_default()
            .to_owned()
    };
    for name in ["audio_crc32=", "state_crc32="] {
        assert_ne!(
            field(&digest(&first), name),
            field(&digest(&later), name),
            "one more frame changes {name}"
        );
    }
}

#[test]
fn time_adds_a_line_of_its_own() {
    let scratch = Scratch::new("time");
    let rom = scratch.write("count.gb", &counting_rom());
    let timed = stdout(&headless(&["--frames", "5", "--time", &rom]));
    let last = timed.lines().last().unwrap_or_default();
    let fields: Vec<&str> = last.split(' ').collect();
    assert_eq!(fields.len(), 3, "{last}");
    for (field, name) in fields.iter().zip(["elapsed_ms=", "ms_per_frame=", "fps="]) {
        let value = field.strip_prefix(name).unwrap_or_else(|| panic!("{last}"));
        assert!(value.parse::<f64>().is_ok(), "{last}");
    }
    let plain = stdout(&headless(&["--frames", "5", &rom]));
    assert_eq!(without_time(&timed), plain);
}

#[test]
fn a_run_from_a_state_carries_on_where_it_was_saved() {
    let scratch = Scratch::new("state");
    let bytes = counting_rom();
    let rom = scratch.write("count.gb", &bytes);
    let state = scratch.write("count.ss1", &state_after(&bytes, 10));

    let whole = stdout(&headless(&["--frames", "15", "--digest", &rom]));
    let resumed = stdout(&headless(&[
        "--frames", "5", "--digest", "--state", &state, &rom,
    ]));
    // The frame counts differ; the screen and the state after do not.
    let state_crc = |text: &str| {
        text.lines()
            .last()
            .and_then(|line| line.split(' ').nth(1))
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(state_crc(&whole), state_crc(&resumed), "{whole}\n{resumed}");
    let screen_crc = |text: &str| {
        text.lines()
            .next()
            .and_then(|line| line.rsplit(' ').next())
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(screen_crc(&whole), screen_crc(&resumed));
}

#[test]
fn a_state_for_another_rom_is_refused() {
    let scratch = Scratch::new("other");
    let rom = scratch.write("count.gb", &counting_rom());
    let mut other = counting_rom();
    other[0x150] = 0xFF;
    let state = scratch.write("other.ss1", &state_after(&other, 1));
    let output = headless(&["--state", &state, &rom]);
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("saved with another ROM"), "{error}");
}
