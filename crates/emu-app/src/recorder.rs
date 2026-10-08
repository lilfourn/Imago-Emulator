//! Video recording: every emulated frame and the sound it made, written to
//! a QuickTime movie (`mov.rs`) on a thread of its own.
//!
//! The emulation thread hands over a copy of each finished frame's screens
//! and sound. The movie runs at the console's own frame rate whatever the
//! emulation speed, so fast-forwarded play comes out at normal speed, and
//! moments that never ran (paused, rewound, before a state was loaded)
//! leave a cut. A short queue absorbs hiccups; if the writer falls further
//! behind, the emulation waits for it rather than drop a frame.
//!
//! Pictures are the screens as the core drew them, laid out as for a
//! screenshot (`capture.rs`). Photo-JPEG keeps 4:2:0 colour, so native
//! frames are first doubled: each console pixel keeps its own colour. DS
//! frames drawn at a higher 3D resolution are large enough already. The
//! movie keeps the size it started at; frames that change size later are
//! resized to it.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use emu_core::{CoreInfo, Frame, FrameRate, ScreenSize, StereoSample};
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};

use crate::capture;
use crate::mov::{self, MovWriter};

/// Frames the writer may fall behind before the emulation waits for it.
const QUEUE_FRAMES: usize = 16;

/// JPEG quality, 1 to 100: high enough that pixel art keeps clean edges.
const JPEG_QUALITY: u8 = 90;

/// A recording in progress. Dropping it finishes the movie.
pub struct Recorder {
    path: PathBuf,
    moments: Option<SyncSender<Moment>>,
    writer: Option<JoinHandle<io::Result<Recording>>>,
}

/// A finished recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recording {
    /// Video frames in the movie.
    pub frames: u64,
    /// How long the movie plays.
    pub duration: Duration,
}

/// One emulated frame: each screen, and the sound made while it ran.
struct Moment {
    screens: Vec<Frame>,
    sound: Vec<StereoSample>,
}

impl Recorder {
    /// Starts recording a game on a core described by `info`, whose
    /// screens are now `screens`, to a new movie at `path`.
    ///
    /// # Errors
    ///
    /// Fails if `path` exists or cannot be created.
    pub fn start(path: PathBuf, info: &CoreInfo, screens: &[Frame]) -> io::Result<Self> {
        let size = video_size(info, screens);
        let (timescale, frame_duration) = time_base(info.frame_rate);
        let video = mov::Video {
            width: size.width,
            height: size.height,
            timescale,
            frame_duration,
        };
        let file = File::create_new(&path)?;
        let movie = MovWriter::new(BufWriter::new(file), video, info.audio_sample_rate)?;
        let players = info.players;
        let (moments, receiver) = mpsc::sync_channel(QUEUE_FRAMES);
        let writer = thread::Builder::new()
            .name("recording".to_owned())
            .spawn(move || write(movie, &receiver, players, size))?;
        Ok(Self {
            path,
            moments: Some(moments),
            writer: Some(writer),
        })
    }

    /// The movie's file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds a frame with its `screens` and the `sound` made while it ran,
    /// waiting if the writer is far behind. Returns false if the writer
    /// stopped after an error, which [`Recorder::finish`] returns.
    pub fn push(&self, screens: &[Frame], sound: &[StereoSample]) -> bool {
        let Some(moments) = &self.moments else {
            return false;
        };
        let moment = Moment {
            screens: screens.to_vec(),
            sound: sound.to_vec(),
        };
        moments.send(moment).is_ok()
    }

    /// Writes the frames still queued and finishes the movie.
    ///
    /// # Errors
    ///
    /// Returns the error that stopped the writer, if any.
    pub fn finish(mut self) -> io::Result<Recording> {
        self.stop()
    }

    fn stop(&mut self) -> io::Result<Recording> {
        // Closing the queue lets the writer finish.
        self.moments = None;
        match self.writer.take().map(JoinHandle::join) {
            Some(Ok(result)) => result,
            _ => Err(io::Error::other("the recording thread stopped")),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.writer.is_some() {
            self.stop().ok();
        }
    }
}

/// The movie's picture size: the screens laid out as one picture, doubled
/// if they are at the console's own resolution.
fn video_size(info: &CoreInfo, screens: &[Frame]) -> ScreenSize {
    let picture = capture::compose(screens, info.players).size();
    let native = match (screens.first(), info.screens.first()) {
        (Some(frame), Some(size)) => frame.size() == *size,
        _ => true,
    };
    let factor = if native { 2 } else { 1 };
    let scale = |length: u16| length.saturating_mul(factor).max(2);
    ScreenSize::new(scale(picture.width), scale(picture.height))
}

/// The video's time scale and frame duration: the console's exact frame
/// rate, as a fraction in lowest terms.
fn time_base(rate: FrameRate) -> (u32, u32) {
    let (mut a, mut b) = (rate.clock_hz, rate.cycles_per_frame);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let divisor = a.max(1);
    (
        rate.clock_hz / divisor,
        (rate.cycles_per_frame / divisor).max(1),
    )
}

/// `picture` as a baseline JPEG with 4:2:0 colour, the kind QuickTime
/// decodes.
fn encode_jpeg(picture: &Frame, out: &mut Vec<u8>) -> io::Result<()> {
    let size = picture.size();
    let bgra: Vec<u8> = picture
        .pixels()
        .iter()
        .flat_map(|pixel| pixel.to_le_bytes())
        .collect();
    let mut encoder = Encoder::new(out, JPEG_QUALITY);
    encoder.set_sampling_factor(SamplingFactor::F_2_2);
    encoder
        .encode(&bgra, size.width, size.height, ColorType::Bgra)
        .map_err(io::Error::other)
}

/// The writer thread: encodes each frame from `moments` until the queue
/// closes, then finishes the movie.
fn write(
    mut movie: MovWriter<BufWriter<File>>,
    moments: &Receiver<Moment>,
    players: usize,
    size: ScreenSize,
) -> io::Result<Recording> {
    let mut jpeg = Vec::new();
    for moment in moments {
        let picture = capture::resize(&capture::compose(&moment.screens, players), size);
        jpeg.clear();
        encode_jpeg(&picture, &mut jpeg)?;
        movie.push(&jpeg, &moment.sound)?;
    }
    let recording = Recording {
        frames: movie.frames(),
        duration: movie.duration(),
    };
    movie.finish()?;
    Ok(recording)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use emu_core::{System, rgb};

    use super::*;

    const DS_SCREENS: [ScreenSize; 2] = [ScreenSize::new(8, 6), ScreenSize::new(8, 6)];

    fn info() -> CoreInfo {
        CoreInfo {
            system: System::Nds,
            screens: &DS_SCREENS,
            touch_screen: Some(1),
            players: 1,
            frame_rate: FrameRate::new(33_513_982, 560_190),
            audio_sample_rate: 32_768,
            state_version: 1,
        }
    }

    fn screens(scale: u16, colour: u32) -> Vec<Frame> {
        DS_SCREENS
            .iter()
            .map(|size| {
                let mut frame =
                    Frame::new(ScreenSize::new(size.width * scale, size.height * scale));
                frame.pixels_mut().fill(colour);
                frame
            })
            .collect()
    }

    #[test]
    fn the_time_base_is_the_exact_frame_rate() {
        assert_eq!(
            time_base(FrameRate::new(4_194_304, 70_224)),
            (262_144, 4389)
        );
        assert_eq!(
            time_base(FrameRate::new(33_513_982, 560_190)),
            (16_756_991, 280_095)
        );
    }

    #[test]
    fn native_pictures_are_doubled_and_upscaled_ones_kept() {
        assert_eq!(video_size(&info(), &screens(1, 0)), ScreenSize::new(16, 24));
        assert_eq!(video_size(&info(), &screens(4, 0)), ScreenSize::new(32, 48));
    }

    #[test]
    fn every_pushed_frame_and_its_sound_reach_the_movie() {
        let dir = std::env::temp_dir().join(format!("emu-app-recorder-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("game.mov");
        let recorder = Recorder::start(path.clone(), &info(), &screens(1, 0)).expect("start");
        assert!(
            Recorder::start(path.clone(), &info(), &screens(1, 0)).is_err(),
            "an existing movie is never overwritten"
        );
        for frame in 0..30 {
            let colour = rgb(frame * 8, 0, 0);
            // The 3D resolution changes partway through.
            let scale = if frame < 10 { 1 } else { 2 };
            assert!(recorder.push(&screens(scale, colour), &[[1, 2]; 548]));
        }
        let recording = recorder.finish().expect("finish");
        assert_eq!(recording.frames, 30);
        // 30 frames at 59.83 per second.
        assert_eq!(recording.duration.as_millis(), 501);

        let bytes = fs::read(&path).expect("the movie");
        let count = |pattern: &[u8]| {
            bytes
                .windows(pattern.len())
                .filter(|w| *w == pattern)
                .count()
        };
        // Each frame is a JPEG: start-of-image, then the JFIF header.
        assert_eq!(count(&[0xFF, 0xD8, 0xFF, 0xE0]), 30);
        assert_eq!(count(b"moov"), 1, "the index is written");
        // All the sound is there: 548 frames of (1, 2) each time.
        let sound = [1u8, 0, 2, 0].repeat(548);
        assert_eq!(count(&sound), 30);
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
