//! The headless runner with `--script`, on a Game Boy ROM made here: a
//! jump to itself, with the screen on and blank.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

/// A 32 KiB ROM whose code is `JR -2` at the entry point.
fn looping_rom() -> Vec<u8> {
    let mut rom = vec![0; 0x8000];
    rom[0x100..0x102].copy_from_slice(&[0x18, 0xFE]);
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

/// A fresh folder holding the ROM, and `script` as `name`.
fn setup(test: &str, name: &str, script: &str) -> (Scratch, String, String) {
    let dir = std::env::temp_dir().join(format!("emu-headless-{test}-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("creating a scratch folder");
    let rom = dir.join("loop.gb");
    fs::write(&rom, looping_rom()).expect("writing the ROM");
    let path = dir.join(name);
    fs::write(&path, script).expect("writing the script");
    let text = |path: PathBuf| path.to_str().expect("a UTF-8 path").to_owned();
    (Scratch(dir), text(rom), text(path))
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

/// Counts in work RAM by random steps, and from frame 5 paints the tile
/// that fills the screen black, so the screen's CRC shows the script ran.
const PAINTER: &str = r#"
emu.on_frame(function()
    local count = memory.read_u8(0xC000)
    memory.write_u8(0xC000, (count + math.random(1, 3)) % 256)
    if emu.frame() == 5 then
        for i = 0, 15 do memory.write_u8(0x8000 + i, 0xFF) end
        emu.save_state(1)
    end
    if emu.frame() == 9 then emu.load_state(1) end
    print(("frame=%d count=%d"):format(emu.frame(), count))
end)
"#;

#[test]
fn the_same_script_and_rom_give_the_same_frames() {
    let (_dir, rom, script) = setup("same", "painter.lua", PAINTER);
    let args = ["--frames", "12", "--script", &script, &rom];
    let first = stdout(&headless(&args));
    let second = stdout(&headless(&args));
    assert_eq!(first, second);

    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 13, "{first}");
    assert!(lines[0].starts_with("frame=1 count=0"), "{first}");
    assert!(lines[11].starts_with("frame=12 count="), "{first}");
    assert!(
        lines[12].starts_with("system=gb frames=12 screen=0"),
        "{first}"
    );

    let plain = stdout(&headless(&["--frames", "12", &rom]));
    let crc = |text: &str| text.lines().last().unwrap_or_default().to_owned();
    assert_ne!(crc(&first), crc(&plain), "the script painted the screen");
}

#[test]
fn pausing_ends_the_run() {
    let source = "emu.on_frame(function() if emu.frame() == 3 then emu.pause() end end)";
    let (_dir, rom, script) = setup("pause", "pause.lua", source);
    let output = headless(&["--frames", "60", "--script", &script, &rom]);
    assert!(stdout(&output).starts_with("system=gb frames=3 "));
}

#[test]
fn a_script_error_fails_the_run_after_its_output() {
    let source = "print('before')\nemu.on_frame(function()\n  error('boom')\nend)";
    let (_dir, rom, script) = setup("error", "broken.lua", source);
    let output = headless(&["--script", &script, &rom]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "emu-headless: broken.lua:3: boom\n"
    );
}

#[test]
fn the_example_scripts_run() {
    let (_dir, rom, _) = setup("examples", "unused.lua", "");
    let example = |name: &str| format!("{}/../../scripts/lua/{name}", env!("CARGO_MANIFEST_DIR"));
    let counter = stdout(&headless(&[
        "--frames",
        "10",
        "--script",
        &example("frame-counter.lua"),
        &rom,
    ]));
    assert!(
        counter.starts_with("frame=1 address=0000C000 value=00\n"),
        "{counter}"
    );
    let macro_run = stdout(&headless(&[
        "--frames",
        "100",
        "--script",
        &example("input-macro.lua"),
        &rom,
    ]));
    assert!(
        macro_run.starts_with("macro=done frame=92\n"),
        "{macro_run}"
    );
}
