//! A real mkxp-z plays a tiny RPG Maker XP game of this test's own, end to
//! end: frames come, buttons move the game's square, a save state goes
//! back to where it was saved, and sound comes.
//!
//! It runs on Linux and Windows when `DS_EMULATOR_MKXP_Z` names a folder
//! holding an mkxp-z build for the system, as it comes (such as
//! `mkxp-z.x86_64` on Linux, or `mkxp-z.x86_64.exe` on Windows), or is
//! `hosted`: then the game starts as a player's first game does, with the
//! hosted build downloaded into its own temporary folders. It is skipped
//! otherwise, saying so. On Linux it needs Mesa's OpenGL and Xvfb or an X
//! display. On macOS the player is the game's own app, which this test does
//! not make. Should it fail, it prints the player's logs.

use std::fs;
use std::path::{Path, PathBuf};

use emu_core::{Button, Buttons, Core, Input, StereoSample, rgb};
use emu_mkxp::{Mkxp, SHARED_PLAYER, UNSUPPORTED, downloads_player};

/// The game: a dark blue screen with a yellow 32×32 square at
/// `$pos`, moved 4 pixels a frame by the buttons the bridge says are held.
const MAIN: &str = r"
$pos ||= [100, 100]
if $ds_loaded
  $pos = Marshal.load($ds_loaded)
  $ds_loaded = nil
end
bitmap = Bitmap.new(640, 480)
sprite = Sprite.new
sprite.bitmap = bitmap
loop do
  $pos[0] += 4 if DSEmulator.pressed?(:right)
  $pos[0] -= 4 if DSEmulator.pressed?(:left)
  $pos[1] += 4 if DSEmulator.pressed?(:down)
  $pos[1] -= 4 if DSEmulator.pressed?(:up)
  bitmap.fill_rect(bitmap.rect, Color.new(0, 0, 64))
  bitmap.fill_rect($pos[0], $pos[1], 32, 32, Color.new(255, 200, 0))
  Graphics.update
end
";

/// The game's compatibility script: its save states are the square's
/// place.
const PRELOAD: &str = r"
DSEmulator.on_save_state { Marshal.dump($pos) }
DSEmulator.on_load_state { |state| $ds_loaded = state }
";

#[test]
fn a_real_mkxp_z_plays_takes_buttons_and_states_and_sounds() {
    let Some(build) = std::env::var_os("DS_EMULATOR_MKXP_Z").map(PathBuf::from) else {
        eprintln!("skipped: DS_EMULATOR_MKXP_Z names no mkxp-z build");
        return;
    };
    if let Some(why) = UNSUPPORTED {
        eprintln!("skipped: {why}");
        return;
    }
    let dir = std::env::temp_dir().join(format!("emu-mkxp-real-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let game = make_game(&dir.join("Square"));
    let work = dir.join("work");
    let _logs = Logs(work.clone());
    if build.as_os_str() == "hosted" {
        assert!(
            downloads_player(&game, &work),
            "the first game downloads mkxp-z"
        );
    } else {
        copy_build(&build, &work.join(SHARED_PLAYER));
    }

    let mut core = Mkxp::open(&game, &work).unwrap_or_else(|error| panic!("{error}"));
    assert!(!downloads_player(&game, &work), "and the next one need not");
    let mut audio = Vec::new();
    assert_eq!(run(&mut core, &mut audio, &[], 5), Some((100, 100)));
    assert_eq!(
        run(&mut core, &mut audio, &[Button::Right], 10),
        Some((140, 100))
    );
    let mut state = Vec::new();
    core.try_save_state(&mut state).expect("saved");
    assert_eq!(
        run(&mut core, &mut audio, &[Button::Down], 5),
        Some((140, 120))
    );
    core.load_state(&state).expect("loaded");
    assert_eq!(
        run(&mut core, &mut audio, &[], 2),
        Some((140, 100)),
        "back where it was saved"
    );
    // OpenAL Soft writes, silence or not, as it plays.
    for _ in 0..100 {
        if !audio.is_empty() {
            break;
        }
        run(&mut core, &mut audio, &[], 1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!audio.is_empty(), "the game's sound comes");
    drop(core);
    fs::remove_dir_all(&dir).ok();
}

/// Runs `frames` frames with `held` held, its sound into `audio`: where
/// the square is then.
fn run(
    core: &mut Mkxp,
    audio: &mut Vec<StereoSample>,
    held: &[Button],
    frames: usize,
) -> Option<(usize, usize)> {
    let input = Input {
        buttons: held.iter().copied().collect::<Buttons>(),
        ..Input::default()
    };
    for _ in 0..frames {
        core.run_frame(&[input], audio);
        assert_eq!(core.ended(), None);
    }
    square(core)
}

/// Prints the players' logs in the work folder it names, if the test fails.
struct Logs(PathBuf);

impl Drop for Logs {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let players = fs::read_dir(&self.0).into_iter().flatten().flatten();
        for player in players.filter(|entry| entry.path().is_dir()) {
            for log in ["player.log", "player-errors.log", "display.log"] {
                let path = player.path().join(log);
                if let Ok(text) = fs::read_to_string(&path) {
                    eprintln!("--- {}\n{text}", path.display());
                }
            }
        }
    }
}

/// Where the square's top-left corner is on the screen.
fn square(core: &Mkxp) -> Option<(usize, usize)> {
    let screen = &core.screens()[0];
    let width = usize::from(screen.size().width);
    let at = screen
        .pixels()
        .iter()
        .position(|&pixel| pixel == rgb(255, 200, 0))?;
    Some((at % width, at / width))
}

/// Writes the game to `folder`: what RPG Maker XP makes, with a `Game.exe`
/// in whose place mkxp-z plays.
fn make_game(folder: &Path) -> PathBuf {
    fs::create_dir_all(folder.join("Data")).expect("game folder");
    fs::write(
        folder.join("Game.ini"),
        "[Game]\r\nLibrary=RGSS104E.dll\r\nScripts=Data\\Scripts.rxdata\r\nTitle=Square\r\n",
    )
    .expect("Game.ini");
    fs::write(folder.join("Game.exe"), b"").expect("Game.exe");
    fs::write(
        folder.join("Data/Scripts.rxdata"),
        scripts(&[(1, "Main", MAIN)]),
    )
    .expect("scripts");
    fs::write(folder.join("ds_test.rb"), PRELOAD).expect("preload");
    fs::write(
        folder.join("mkxp.json"),
        r#"{ "rgssVersion": 1, "pathCache": false, "preloadScript": ["ds_test.rb"] }"#,
    )
    .expect("mkxp.json");
    folder.to_owned()
}

/// Copies the files of the mkxp-z build `from` to `to`, keeping their
/// permissions.
fn copy_build(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("players' folder");
    for entry in fs::read_dir(from).expect("the build's folder") {
        let path = entry.expect("entry").path();
        if path.is_file() {
            fs::copy(&path, to.join(path.file_name().expect("name"))).expect("copied");
        }
    }
}

/// `Data/Scripts.rxdata`: RPG Maker's scripts, Ruby's `Marshal` of an
/// array of `[id, name, zlib-compressed code]`.
fn scripts(scripts: &[(i64, &str, &str)]) -> Vec<u8> {
    let mut out = vec![4, 8, b'['];
    marshal_int(&mut out, scripts.len() as i64);
    for (id, name, code) in scripts {
        out.push(b'[');
        marshal_int(&mut out, 3);
        out.push(b'i');
        marshal_int(&mut out, *id);
        marshal_string(&mut out, name.as_bytes());
        marshal_string(&mut out, &zlib_stored(code.as_bytes()));
    }
    out
}

/// A non-negative integer as `Marshal` writes one.
fn marshal_int(out: &mut Vec<u8>, n: i64) {
    match n {
        0 => out.push(0),
        1..=122 => out.push(n as u8 + 5),
        _ => {
            let bytes = n.to_le_bytes();
            let count = 8 - bytes.iter().rev().take_while(|&&byte| byte == 0).count();
            out.push(count as u8);
            out.extend(&bytes[..count]);
        }
    }
}

/// A string of bytes as Ruby 1.8's `Marshal` writes one.
fn marshal_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'"');
    marshal_int(out, bytes.len() as i64);
    out.extend(bytes);
}

/// `data` as a zlib stream of stored, uncompressed blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = data.chunks(0xFFFF).collect();
    for (index, block) in blocks.iter().enumerate() {
        out.push(u8::from(index + 1 == blocks.len()));
        let size = block.len() as u16;
        out.extend(size.to_le_bytes());
        out.extend((!size).to_le_bytes());
        out.extend(*block);
    }
    let (mut a, mut b) = (1_u32, 0_u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    out.extend(((b << 16) | a).to_be_bytes());
    out
}
