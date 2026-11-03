//! Plays a 3DS game headless for a number of frames, as fast as it runs,
//! and reports the time each frame and state took. For measuring the core
//! and the link; the app and emu-headless play games for real.
//!
//! `cargo run --release -p emu-3ds --example play -- GAME FRAMES [OUT_DIR]`
//!
//! The core and MoltenVK come from `DS_EMULATOR_3DS_CORE` and
//! `DS_EMULATOR_3DS_MOLTENVK`, or the installed ones. Every 600th frame is
//! written to OUT_DIR as two PPM images.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use emu_core::{Button, Buttons, Core, Frame, Input};

fn main() {
    if let Some(code) = emu_3ds::run_helper_if_asked() {
        std::process::exit(code);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(game), Some(frames)) = (args.first(), args.get(1).and_then(|n| n.parse().ok()))
    else {
        eprintln!("usage: play GAME FRAMES [OUT_DIR]");
        std::process::exit(2);
    };
    let out = args.get(2).map(PathBuf::from);
    if let Err(error) = play(Path::new(game), frames, out.as_deref()) {
        eprintln!("play: {error}");
        std::process::exit(1);
    }
}

fn play(game: &Path, frames: u64, out: Option<&Path>) -> Result<(), String> {
    let data = std::env::var_os("DS_EMULATOR_3DS_DATA").map_or_else(
        || std::env::temp_dir().join(format!("emu-3ds-play-{}", std::process::id())),
        PathBuf::from,
    );
    let helper = std::env::current_exe().map_err(|error| error.to_string())?;
    let parts = match (
        std::env::var_os("DS_EMULATOR_3DS_CORE"),
        std::env::var_os("DS_EMULATOR_3DS_MOLTENVK"),
    ) {
        (Some(core), Some(moltenvk)) => emu_3ds::install::Parts {
            core: core.into(),
            moltenvk: moltenvk.into(),
        },
        _ => {
            let installed = emu_3ds::data_dir().ok_or("no data folder")?;
            emu_3ds::install::ensure(&installed).map_err(|error| error.to_string())?
        }
    };
    let started = Instant::now();
    let mut core = emu_3ds::ThreeDs::open_with(game, &data, &helper, &parts)
        .map_err(|error| error.to_string())?;
    println!("started in {:.2} s", started.elapsed().as_secs_f64());
    if let Some(scale) = std::env::var("DS_EMULATOR_3DS_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        core.set_render_scale(scale);
    }

    let mut audio = Vec::new();
    let mut sound = 0usize;
    let mut slowest = Duration::ZERO;
    let begun = Instant::now();
    for frame in 0..frames {
        // Tap A for 6 frames in every 90 from frame 300, to pass title screens.
        let held: Buttons = (frame >= 300 && frame % 90 < 6)
            .then_some(Button::A)
            .into_iter()
            .collect();
        let input = Input {
            buttons: held,
            ..Input::default()
        };
        audio.clear();
        let one = Instant::now();
        core.run_frame(&[input], &mut audio);
        let took = one.elapsed();
        if frame > 60 {
            slowest = slowest.max(took);
        }
        sound += audio.len();
        if let Some(why) = core.ended() {
            return Err(format!("the game ended at frame {frame}: {why}"));
        }
        if (frame + 1) % 600 == 0 {
            println!(
                "frame {}: {:.1} fps so far",
                frame + 1,
                (frame + 1) as f64 / begun.elapsed().as_secs_f64()
            );
            if let Some(out) = out {
                for (index, screen) in core.screens().iter().enumerate() {
                    write_ppm(
                        &out.join(format!("frame-{:05}-{index}.ppm", frame + 1)),
                        screen,
                    )?;
                }
            }
        }
    }
    let total = begun.elapsed().as_secs_f64();
    println!(
        "{frames} frames in {total:.1} s: {:.1} fps ({:.0}% of 59.83), slowest {:.1} ms, sound {:.2} s",
        frames as f64 / total,
        frames as f64 / total / 59.83 * 100.0,
        slowest.as_secs_f64() * 1000.0,
        sound as f64 / f64::from(emu_3ds::SAMPLE_RATE),
    );
    let sizes: Vec<String> = core
        .screens()
        .iter()
        .map(|screen| format!("{}x{}", screen.size().width, screen.size().height))
        .collect();
    println!("screens {}", sizes.join(" and "));

    let mut state = Vec::new();
    let saved = Instant::now();
    core.try_save_state(&mut state)?;
    println!(
        "state {:.1} MB saved in {:.0} ms",
        state.len() as f64 / 1e6,
        saved.elapsed().as_secs_f64() * 1000.0
    );
    for _ in 0..60 {
        core.run_frame(&[Input::default()], &mut audio);
    }
    let loaded = Instant::now();
    core.load_state(&state).map_err(|error| error.to_string())?;
    println!(
        "state loaded in {:.0} ms",
        loaded.elapsed().as_secs_f64() * 1000.0
    );
    core.run_frame(&[Input::default()], &mut audio);
    if let Some(why) = core.ended() {
        return Err(format!("the game ended after loading: {why}"));
    }
    drop(core);
    if std::env::var_os("DS_EMULATOR_3DS_DATA").is_none() {
        let _ = std::fs::remove_dir_all(&data);
    }
    Ok(())
}

fn write_ppm(path: &Path, frame: &Frame) -> Result<(), String> {
    let size = frame.size();
    let mut bytes = format!("P6\n{} {}\n255\n", size.width, size.height).into_bytes();
    for pixel in frame.pixels() {
        bytes.extend([(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]);
    }
    std::fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}
