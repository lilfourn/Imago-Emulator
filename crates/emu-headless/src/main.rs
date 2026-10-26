//! Runs a core without a window and prints one line per screen, for test-ROM
//! automation and trace diffing.
//!
//! ```text
//! emu-headless [--system gb|gba|nds] [--frames N] [--start-time SECS] [--dump DIR] [--wav FILE]
//!              [--script FILE.lua] [--work-dir DIR] ROM
//! ```
//!
//! An RPG Maker game's folder plays in place of a ROM, in a hidden player
//! of its own kept in `--work-dir` (by default
//! `~/Library/Caches/emu-headless`), apart from the app's.
//!
//! Output is plain `key=value` text, one line per screen:
//!
//! ```text
//! system=gba frames=60 screen=0 width=240 height=160 crc32=8f2e44c1
//! ```
//!
//! With `--script`, a Lua script runs alongside the game (see
//! `docs/scripting.md`). What it prints comes first, as it prints it, so a
//! script can write `key=value` lines of its own. `emu.pause()` ends the
//! run after that frame, and `frames=` then says how many ran. Save-state
//! slots live in memory for the run. Drawing is ignored. A script error
//! ends the run with exit status 1.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use emu_core::debug::Stops;
use emu_core::{Core, Frame, Input, Media, StereoSample, System};
use emu_script::{Output, Script, StateRequest};

const USAGE: &str = "usage: emu-headless [--system gb|gba|nds] [--frames N] [--start-time SECS] \
                     [--dump DIR] [--wav FILE] [--script FILE.lua] [--work-dir DIR] ROM";

struct Args {
    rom: PathBuf,
    system: Option<System>,
    frames: u64,
    start_time: i64,
    dump: Option<PathBuf>,
    wav: Option<PathBuf>,
    script: Option<PathBuf>,
    work_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("emu-headless: {error:#}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("emu-headless: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(mut argv: impl Iterator<Item = String>) -> anyhow::Result<Args> {
    let mut rom = None;
    let mut system = None;
    let mut frames = 60;
    let mut start_time = 0;
    let mut dump = None;
    let mut wav = None;
    let mut script = None;
    let mut work_dir = None;
    while let Some(arg) = argv.next() {
        let mut value = |name: &str| argv.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--system" => system = Some(value("--system")?.parse()?),
            "--frames" => frames = value("--frames")?.parse().context("--frames")?,
            "--start-time" => {
                start_time = value("--start-time")?.parse().context("--start-time")?
            }
            "--dump" => dump = Some(PathBuf::from(value("--dump")?)),
            "--wav" => wav = Some(PathBuf::from(value("--wav")?)),
            "--script" => script = Some(PathBuf::from(value("--script")?)),
            "--work-dir" => work_dir = Some(PathBuf::from(value("--work-dir")?)),
            "-h" | "--help" => bail!("help requested"),
            flag if flag.starts_with('-') => bail!("unknown option {flag}"),
            path if rom.is_none() => rom = Some(PathBuf::from(path)),
            extra => bail!("unexpected argument {extra}"),
        }
    }
    Ok(Args {
        rom: rom.context("missing ROM path")?,
        system,
        frames,
        start_time,
        dump,
        wav,
        script,
        work_dir,
    })
}

fn run(args: &Args) -> anyhow::Result<()> {
    let (system, mut core) = open(args)?;

    let mut out = BufWriter::new(io::stdout().lock());
    let mut audio = Vec::new();
    let frames = match &args.script {
        Some(path) => run_script(path, &mut *core, args, &mut audio, &mut out)?,
        None => {
            let inputs = vec![Input::default(); core.info().players.max(1)];
            for _ in 0..args.frames {
                let keep = audio.len();
                core.run_frame(&inputs, &mut audio);
                if args.wav.is_none() {
                    audio.truncate(keep);
                }
            }
            args.frames
        }
    };
    if let Some(path) = &args.wav {
        write_wav(path, core.info().audio_sample_rate, &audio)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    for (index, screen) in core.screens().iter().enumerate() {
        let size = screen.size();
        let crc32 = crc32fast::hash(&bgra_bytes(screen));
        writeln!(
            out,
            "system={system} frames={frames} screen={index} width={} height={} crc32={crc32:08x}",
            size.width, size.height,
        )?;
        if let Some(dir) = &args.dump {
            let path = dir.join(format!("screen{index}.ppm"));
            write_ppm(&path, screen).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    out.flush()?;
    Ok(())
}

/// The system and a core playing the game `args` names: a ROM, or an RPG
/// Maker game's folder.
fn open(args: &Args) -> anyhow::Result<(System, Box<dyn Core>)> {
    if let Some(game) = emu_cores::rpg_maker::game_folder(&args.rom) {
        let work_dir = match &args.work_dir {
            Some(dir) => dir.clone(),
            None => std::env::home_dir()
                .context("there is no home folder to keep the game's player in; pass --work-dir")?
                .join("Library/Caches/emu-headless"),
        };
        let core = emu_cores::rpg_maker::Mkxp::open(&game, &work_dir)?;
        return Ok((System::RpgMaker, Box::new(core)));
    }
    let system = match args.system {
        Some(system) => system,
        None => emu_cores::system_for_path(&args.rom).with_context(|| {
            format!(
                "cannot tell the system of {}; pass --system",
                args.rom.display()
            )
        })?,
    };
    let rom = fs::read(&args.rom).with_context(|| format!("reading {}", args.rom.display()))?;
    let media = Media {
        start_time: args.start_time,
        ..Media::new(&rom)
    };
    Ok((system, emu_cores::create(system, &media)?))
}

/// Runs up to `args.frames` frames with the script at `path` alongside,
/// writing what it prints to `out`. Returns how many frames ran: fewer if
/// the script paused.
fn run_script(
    path: &Path,
    core: &mut dyn Core,
    args: &Args,
    audio: &mut Vec<StereoSample>,
    out: &mut impl Write,
) -> anyhow::Result<u64> {
    let source = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut script = Script::new(&name, &source)?;
    let started = script.start(core);
    let mut output = script.take_output();
    let mut slots = Slots::default();
    let mut paused = slots.obey(&output, core, out)?;
    started?;

    let players = core.info().players.max(1);
    let mut frames = 0;
    while frames < args.frames && !paused {
        let inputs: Vec<Input> = (0..players)
            .map(|player| output.input(player, Input::default()))
            .collect();
        let keep = audio.len();
        // No stops of our own, so the frame always finishes.
        let (_, called) = script.run_frame(core, &inputs, audio, &Stops::default());
        if args.wav.is_none() {
            audio.truncate(keep);
        }
        frames += 1;
        let result = called.and_then(|()| script.after_frame(core, &inputs));
        output = script.take_output();
        paused = slots.obey(&output, core, out)?;
        result?;
    }
    Ok(frames)
}

/// Save-state slots kept in memory for one run.
#[derive(Default)]
struct Slots(Vec<(u8, Vec<u8>)>);

impl Slots {
    /// Writes the script's log lines to `out` and saves and loads the
    /// states it asked for. Returns whether it asked to pause.
    fn obey(
        &mut self,
        output: &Output,
        core: &mut dyn Core,
        out: &mut impl Write,
    ) -> io::Result<bool> {
        for line in &output.log {
            writeln!(out, "{line}")?;
        }
        for request in &output.states {
            match *request {
                StateRequest::Save(slot) => {
                    let mut state = Vec::new();
                    core.save_state(&mut state);
                    self.0.retain(|(saved, _)| *saved != slot);
                    self.0.push((slot, state));
                }
                StateRequest::Load(slot) => {
                    let loaded = match self.0.iter().find(|(saved, _)| *saved == slot) {
                        Some((_, state)) => {
                            core.load_state(state).map_err(|error| error.to_string())
                        }
                        None => Err("nothing was saved there".to_owned()),
                    };
                    if let Err(error) = loaded {
                        eprintln!("emu-headless: could not load state {slot}: {error}");
                    }
                }
            }
        }
        Ok(output.pause)
    }
}

/// The frame in its in-memory byte order, so hashes match across hosts.
fn bgra_bytes(frame: &Frame) -> Vec<u8> {
    frame
        .pixels()
        .iter()
        .flat_map(|pixel| pixel.to_le_bytes())
        .collect()
}

/// Writes 16-bit stereo PCM as a WAV file.
fn write_wav(path: &Path, sample_rate: u32, samples: &[[i16; 2]]) -> io::Result<()> {
    let data_len =
        u32::try_from(samples.len() * 4).map_err(|_| io::Error::other("audio too long for WAV"))?;
    let mut file = BufWriter::new(fs::File::create(path)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&2u16.to_le_bytes())?; // channels
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&(sample_rate * 4).to_le_bytes())?; // bytes per second
    file.write_all(&4u16.to_le_bytes())?; // bytes per frame
    file.write_all(&16u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    for [left, right] in samples {
        file.write_all(&left.to_le_bytes())?;
        file.write_all(&right.to_le_bytes())?;
    }
    file.flush()
}

/// Writes a binary PPM (P6), viewable in Preview and most image tools.
fn write_ppm(path: &Path, frame: &Frame) -> io::Result<()> {
    let size = frame.size();
    let mut file = BufWriter::new(fs::File::create(path)?);
    write!(file, "P6\n{} {}\n255\n", size.width, size.height)?;
    for pixel in frame.pixels() {
        let [b, g, r, _] = pixel.to_le_bytes();
        file.write_all(&[r, g, b])?;
    }
    file.flush()
}
