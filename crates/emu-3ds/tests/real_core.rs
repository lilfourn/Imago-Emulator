//! Plays a game on this system's pinned 3DS core, downloaded for real,
//! through the helper, as the app does: the core installs, its digest is
//! checked, the helper starts and talks over its pipes, and frames, both
//! screens, the texture filter and save states come back.
//!
//! The game is a few bytes of homebrew made here, which only spins, so no
//! game is downloaded or kept. The helper is this test's own executable,
//! started again, so the test has a `main` of its own (`harness = false`).
//!
//! It downloads the core (12 MB on Windows, 15 MB on Linux, 67 MB on a
//! Mac), so it runs by itself only on Windows, where CI is the only test
//! machine. Elsewhere it is ignored unless asked for, as libtest's ignored
//! tests are:
//!
//! ```text
//! cargo test -p emu-3ds --test real_core -- --ignored
//! ```
//!
//! Where 3DS games don't play (Linux on ARM), it checks that the install
//! refuses, saying why.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use emu_core::{Core, Input, TextureUpscaling};

/// The test's name, for filters and the report.
const NAME: &str = "plays_homebrew_on_the_pinned_core";

fn main() -> ExitCode {
    if let Some(code) = emu_3ds::run_helper_if_asked() {
        std::process::exit(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    if flag("--list") {
        println!("{NAME}: test");
        return ExitCode::SUCCESS;
    }
    // Filters, as libtest takes them: a name must hold one of them.
    let filters: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    if !filters.is_empty() && !filters.iter().any(|filter| NAME.contains(filter.as_str())) {
        println!("\nrunning 0 tests\n");
        return ExitCode::SUCCESS;
    }
    println!("\nrunning 1 test");
    if !cfg!(windows) && !flag("--ignored") && !flag("--include-ignored") {
        println!("test {NAME} ... ignored, downloads the 3DS core: run it with -- --ignored\n");
        return ExitCode::SUCCESS;
    }
    let home = std::env::temp_dir().join(format!("emu-3ds-real-core-{}", std::process::id()));
    let result = play(&home);
    remove(&home);
    match result {
        Ok(()) => {
            println!("test {NAME} ... ok\n");
            ExitCode::SUCCESS
        }
        Err(why) => {
            println!("test {NAME} ... FAILED\n\n{why}\n");
            ExitCode::FAILURE
        }
    }
}

/// Fails with `why` unless `ok`.
fn check(ok: bool, why: impl FnOnce() -> String) -> Result<(), String> {
    if ok { Ok(()) } else { Err(why()) }
}

fn play(home: &Path) -> Result<(), String> {
    let data = home.join("3DS");
    std::fs::create_dir_all(&data).map_err(|error| error.to_string())?;
    if let Some(why) = emu_3ds::UNSUPPORTED {
        let refused = emu_3ds::install::ensure(&data);
        return match refused {
            Err(error) if error.to_string() == why => {
                println!("no 3DS core here, as expected: {why}");
                Ok(())
            }
            other => Err(format!(
                "expected the install to refuse with {why:?}, got {other:?}"
            )),
        };
    }

    let started = Instant::now();
    let parts = emu_3ds::install::ensure(&data).map_err(|error| error.to_string())?;
    println!(
        "installed {} in {:.1?}",
        parts.core.display(),
        started.elapsed()
    );
    check(
        parts.moltenvk.is_some() == cfg!(target_os = "macos"),
        || format!("MoltenVK only on macOS, got {:?}", parts.moltenvk),
    )?;
    check(
        emu_3ds::install::installed(&data) == Some(parts.clone()),
        || "the install is not found again".to_owned(),
    )?;

    let game = home.join("spin.3dsx");
    std::fs::write(&game, spinning_homebrew()).map_err(|error| error.to_string())?;
    let helper = std::env::current_exe().map_err(|error| error.to_string())?;
    let started = Instant::now();
    let mut core =
        emu_3ds::ThreeDs::open(&game, &data, &data, &helper).map_err(|error| error.to_string())?;
    println!("started the helper in {:.1?}", started.elapsed());

    let started = Instant::now();
    run(&mut core, 60)?;
    let elapsed = started.elapsed();
    println!(
        "60 frames in {:.1?} ({:.0} fps)",
        elapsed,
        60.0 / elapsed.as_secs_f64()
    );
    check_screens(&core, 1)?;

    // The texture filter goes through, and the game plays on.
    core.set_texture_upscaling(TextureUpscaling::PixelArt);
    run(&mut core, 2)?;

    // A state saves and loads, but for homebrew under Vulkan, which the
    // helper refuses (decision 0011).
    let mut state = Vec::new();
    match core.try_save_state(&mut state) {
        Ok(()) => {
            println!("saved a state of {} bytes", state.len());
            core.load_state(&state)
                .map_err(|error| format!("loading the state: {error}"))?;
            run(&mut core, 10)?;
        }
        Err(why) if why.contains("title ID") => println!("states refused: {why}"),
        Err(why) => return Err(format!("saving a state: {why}")),
    }

    // Twice the resolution on the GPU; the software renderer stays at 1×.
    core.set_render_scale(2);
    run(&mut core, 3)?;
    let scale = u32::from(core.screens()[0].size().width) / 400;
    check_screens(&core, scale)?;
    println!("at 2×, the screens came out {scale}×");
    drop(core);

    let log =
        std::fs::read_to_string(data.join("helper.log")).map_err(|error| error.to_string())?;
    let renderer = log
        .lines()
        .find(|line| line.contains("3DS helper: playing"))
        .ok_or_else(|| format!("the helper's log says nothing of playing:\n{log}"))?;
    println!("{renderer}");
    Ok(())
}

/// Runs `frames` frames, holding nothing, failing if the helper stops.
fn run(core: &mut emu_3ds::ThreeDs, frames: u32) -> Result<(), String> {
    let mut audio = Vec::new();
    for _ in 0..frames {
        core.run_frame(&[Input::default()], &mut audio);
        if let Some(why) = core.ended() {
            return Err(why);
        }
    }
    Ok(())
}

/// Checks the screens are the 3DS's, `scale` times as large.
fn check_screens(core: &emu_3ds::ThreeDs, scale: u32) -> Result<(), String> {
    let sizes: Vec<_> = core.screens().iter().map(emu_core::Frame::size).collect();
    let expected: Vec<_> = emu_3ds::SCREENS
        .iter()
        .map(|size| {
            let times = |side: u16| u16::try_from(u32::from(side) * scale).unwrap_or(u16::MAX);
            emu_core::ScreenSize::new(times(size.width), times(size.height))
        })
        .collect();
    check(sizes == expected, || {
        format!("screens {sizes:?}, expected {expected:?}")
    })
}

/// A 3DSX homebrew whose whole program is one ARM instruction branching to
/// itself (`b .`): the 3DSX header, three empty relocation headers, and
/// the code.
fn spinning_homebrew() -> Vec<u8> {
    let code = 0xEAFF_FFFE_u32.to_le_bytes();
    let mut file = b"3DSX".to_vec();
    // The header's size, the relocation headers' size, the format, flags.
    file.extend(0x20_u16.to_le_bytes());
    file.extend(8_u16.to_le_bytes());
    file.extend(0_u32.to_le_bytes());
    file.extend(0_u32.to_le_bytes());
    // The code, read-only data, data and BSS segments' sizes.
    file.extend(u32::try_from(code.len()).unwrap_or(4).to_le_bytes());
    file.extend([0; 12]);
    // No relocations in any of the three segments.
    file.extend([0; 24]);
    file.extend(code);
    file
}

/// Removes `home`, waiting for a helper still exiting to let go of its
/// files, which Windows won't delete while they are open.
fn remove(home: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::remove_dir_all(home).is_err() && home.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}
