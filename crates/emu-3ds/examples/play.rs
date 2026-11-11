//! Plays a 3DS game headless for a number of frames, as fast as it runs,
//! and reports the time each frame and state took. For measuring the core
//! and the link; the app and emu-headless play games for real.
//!
//! `cargo run --release -p emu-3ds --example play -- GAME FRAMES [OUT_DIR]`
//!
//! The core and MoltenVK come from `DS_EMULATOR_3DS_CORE` and
//! `DS_EMULATOR_3DS_MOLTENVK` (the core alone off macOS, where the system's
//! Vulkan loader stands in for MoltenVK), or the installed ones. Every
//! 600th frame is written to OUT_DIR as two PPM images.

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
    let moltenvk = std::env::var_os("DS_EMULATOR_3DS_MOLTENVK");
    let parts = match std::env::var_os("DS_EMULATOR_3DS_CORE") {
        Some(core) if moltenvk.is_some() || !cfg!(target_os = "macos") => emu_3ds::install::Parts {
            core: core.into(),
            moltenvk: moltenvk.map(PathBuf::from),
        },
        _ => {
            let installed = emu_3ds::data_dir().ok_or("no data folder")?;
            emu_3ds::install::ensure(&installed).map_err(|error| error.to_string())?
        }
    };
    let started = Instant::now();
    let mut core = emu_3ds::ThreeDs::open_with(game, &data, &data, &helper, &parts)
        .map_err(|error| error.to_string())?;
    println!("started in {:.2} s", started.elapsed().as_secs_f64());
    if let Some(scale) = std::env::var("DS_EMULATOR_3DS_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        core.set_render_scale(scale);
    }

    // DS_EMULATOR_3DS_PACE=1 runs frames at the 3DS's own rate, as the app
    // does, rather than as fast as they go.
    let paced = std::env::var_os("DS_EMULATOR_3DS_PACE").is_some();
    let mut deadline = Instant::now();
    let mut audio = Vec::new();
    let mut sound = 0usize;
    let mut times = Vec::new();
    let begun = Instant::now();
    for frame in 0..frames {
        // Tap A for 6 frames in every 90 from frame 300, to pass title screens.
        let held: Buttons = (frame >= 300 && frame % 90 < 6)
            .then_some(Button::A)
            .into_iter()
            .collect();
        // DS_EMULATOR_3DS_TOUCH="FROM:TO:X:Y" touches the bottom screen at
        // (X, Y) from frame FROM to frame TO.
        let touch = std::env::var("DS_EMULATOR_3DS_TOUCH")
            .ok()
            .and_then(|spec| {
                let parts: Vec<u64> = spec
                    .split(':')
                    .filter_map(|part| part.parse().ok())
                    .collect();
                let [from, to, x, y] = parts[..] else {
                    return None;
                };
                let point = emu_core::TouchPoint {
                    x: u16::try_from(x).ok()?,
                    y: u16::try_from(y).ok()?,
                };
                (from..to).contains(&frame).then_some(point)
            });
        let input = Input {
            buttons: held,
            touch,
            ..Input::default()
        };
        audio.clear();
        let one = Instant::now();
        core.run_frame(&[input], &mut audio);
        times.push((frame, one.elapsed()));
        sound += audio.len();
        if paced {
            deadline += emu_3ds::FRAME_RATE.frame_duration();
            deadline = deadline.max(Instant::now() - Duration::from_millis(100));
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                if left < Duration::from_micros(200) {
                    break;
                }
                std::thread::sleep(left * 3 / 4);
            }
        }
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
        "{frames} frames in {total:.1} s: {:.1} fps ({:.0}% of 59.83), sound {:.2} s",
        frames as f64 / total,
        frames as f64 / total / 59.83 * 100.0,
        sound as f64 / f64::from(emu_3ds::SAMPLE_RATE),
    );
    report_times(&times);
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

/// Prints how long frames took after the first second: their spread, how
/// many took longer than a frame lasts, and the slowest.
fn report_times(times: &[(u64, Duration)]) {
    let ms = |time: Duration| time.as_secs_f64() * 1000.0;
    let mut sorted: Vec<Duration> = times.iter().skip(60).map(|(_, time)| *time).collect();
    if sorted.is_empty() {
        return;
    }
    sorted.sort();
    let at = |share: f64| ms(sorted[((sorted.len() - 1) as f64 * share) as usize]);
    let frame = emu_3ds::FRAME_RATE.frame_duration();
    let over = sorted.iter().filter(|time| **time > frame).count();
    let stalled: Duration = sorted.iter().map(|time| time.saturating_sub(frame)).sum();
    println!(
        "frame ms: median {:.2}, p90 {:.2}, p99 {:.2}, max {:.2}; {over} of {} over a frame's {:.1} ms, by {:.0} ms in all",
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.0),
        sorted.len(),
        ms(frame),
        ms(stalled),
    );
    let mut slowest: Vec<&(u64, Duration)> = times.iter().skip(60).collect();
    slowest.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
    let list: Vec<String> = slowest
        .iter()
        .take(12)
        .map(|(frame, time)| format!("{frame}: {:.1}", ms(*time)))
        .collect();
    println!("slowest frames (frame: ms): {}", list.join(", "));
}

fn write_ppm(path: &Path, frame: &Frame) -> Result<(), String> {
    let size = frame.size();
    let mut bytes = format!("P6\n{} {}\n255\n", size.width, size.height).into_bytes();
    for pixel in frame.pixels() {
        bytes.extend([(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]);
    }
    std::fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}
