//! Runs a core without a window and prints one line per screen, for test-ROM
//! automation and trace diffing.
//!
//! ```text
//! emu-headless [--system gb|gba|nds] [--frames N] [--start-time SECS] [--dump DIR] [--wav FILE] ROM
//! ```
//!
//! Output is plain `key=value` text, one line per screen:
//!
//! ```text
//! system=gba frames=60 screen=0 width=240 height=160 crc32=8f2e44c1
//! ```

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use emu_core::{Frame, Input, Media, System};

const USAGE: &str = "usage: emu-headless [--system gb|gba|nds] [--frames N] [--start-time SECS] \
                     [--dump DIR] [--wav FILE] ROM";

struct Args {
    rom: PathBuf,
    system: Option<System>,
    frames: u64,
    start_time: i64,
    dump: Option<PathBuf>,
    wav: Option<PathBuf>,
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
    })
}

fn run(args: &Args) -> anyhow::Result<()> {
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
    let mut core = emu_cores::create(system, &media)?;

    let input = Input::default();
    let mut audio = Vec::new();
    for _ in 0..args.frames {
        let keep = audio.len();
        core.run_frame(&[input], &mut audio);
        if args.wav.is_none() {
            audio.truncate(keep);
        }
    }
    if let Some(path) = &args.wav {
        write_wav(path, core.info().audio_sample_rate, &audio)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    let mut out = BufWriter::new(io::stdout().lock());
    for (index, screen) in core.screens().iter().enumerate() {
        let size = screen.size();
        let crc32 = crc32fast::hash(&bgra_bytes(screen));
        writeln!(
            out,
            "system={system} frames={} screen={index} width={} height={} crc32={crc32:08x}",
            args.frames, size.width, size.height,
        )?;
        if let Some(dir) = &args.dump {
            let path = dir.join(format!("screen{index}.ppm"));
            write_ppm(&path, screen).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    out.flush()?;
    Ok(())
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
