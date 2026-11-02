//! The player's sound. mkxp-z plays its sound through OpenAL Soft, which
//! can write what it plays to a WAV file in place of a sound card. Here it
//! writes to a named pipe, in real time, and a thread reads the pipe into a
//! buffer that the core empties after each frame.
//!
//! The sound runs on the player's clock, not the frames': the game's music
//! plays on while a frame takes a tenth of a second to load a map. So a
//! [`Tap`] also hands it on as it comes, for an app to play at once.

use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use emu_core::StereoSample;

/// The rate OpenAL Soft is asked to play at.
pub const SAMPLE_RATE: u32 = 48_000;
/// The most sound kept waiting, a quarter of a second. The player plays on
/// while the game is paused, and the oldest goes.
const MAX_WAITING: usize = SAMPLE_RATE as usize / 4;

type Waiting = Arc<Mutex<VecDeque<StereoSample>>>;

/// Takes the player's sound as it comes, a piece at a time, on the thread
/// that reads it.
pub type Listener = Box<dyn FnMut(&[StereoSample]) + Send>;

/// Where the player's sound goes as it comes, besides the frames: see
/// [`Tap::listen`].
#[derive(Clone, Default)]
pub struct Tap(Arc<Mutex<Option<Listener>>>);

impl Tap {
    /// Hands the player's sound to `listener` as it comes, from now on, in
    /// place of any listener before. The frames still take it too.
    pub fn listen(&self, listener: Listener) {
        *lock(&self.0) = Some(listener);
    }

    /// Gives `samples`, just read, to the listener, if there is one.
    fn hand_on(&self, samples: &[StereoSample]) {
        if let Some(listener) = lock(&self.0).as_mut() {
            listener(samples);
        }
    }
}

impl fmt::Debug for Tap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let listening = lock(&self.0).is_some();
        f.debug_struct("Tap")
            .field("listening", &listening)
            .finish()
    }
}

/// The player's sound, as it comes.
#[derive(Debug)]
pub struct Capture {
    waiting: Waiting,
    tap: Tap,
    pipe: PathBuf,
}

impl Capture {
    /// Makes the named pipe `pipe`, writes OpenAL Soft's settings to
    /// `settings` to send the sound there, and starts reading it.
    ///
    /// # Errors
    ///
    /// Fails if the pipe or the settings cannot be made.
    pub fn start(pipe: &Path, settings: &Path) -> io::Result<Self> {
        match fs::remove_file(pipe) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let made = Command::new("mkfifo").arg(pipe).status()?;
        if !made.success() {
            return Err(io::Error::other(format!(
                "mkfifo could not make {}",
                pipe.display()
            )));
        }
        fs::write(settings, openal_settings(pipe))?;
        let waiting = Waiting::default();
        let tap = Tap::default();
        // The reader stops once the player closes the pipe.
        {
            let (pipe, waiting, tap) = (pipe.to_owned(), Arc::clone(&waiting), tap.clone());
            thread::Builder::new()
                .name("game-sound".to_owned())
                .spawn(move || read_pipe(&pipe, &waiting, &tap))?;
        }
        Ok(Self {
            waiting,
            tap,
            pipe: pipe.to_owned(),
        })
    }

    /// Moves the sound that has come since the last call to `out`.
    pub fn drain(&self, out: &mut Vec<StereoSample>) {
        out.extend(lock(&self.waiting).drain(..));
    }

    /// Where the sound also goes as it comes.
    pub fn tap(&self) -> Tap {
        self.tap.clone()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // A reader still waiting for the player to open the pipe gets in
        // and finds it closed. Opening without waiting fails if no one
        // reads, when there is nothing to stop.
        OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.pipe)
            .ok();
        fs::remove_file(&self.pipe).ok();
    }
}

/// OpenAL Soft's settings to write 16-bit stereo at [`SAMPLE_RATE`] to
/// `pipe`, in its configuration file's format.
fn openal_settings(pipe: &Path) -> String {
    format!(
        "# Written by ds-emulator: the game's sound goes to its pipe.\n\
         [general]\n\
         drivers = wave\n\
         frequency = {SAMPLE_RATE}\n\
         channels = stereo\n\
         sample-type = int16\n\
         \n\
         [wave]\n\
         file = {}\n",
        pipe.display()
    )
}

/// Reads `pipe` into `waiting`, and hands it on through `tap`, until the
/// player closes it.
fn read_pipe(pipe: &Path, waiting: &Waiting, tap: &Tap) {
    let Ok(file) = File::open(pipe) else {
        return;
    };
    let mut reader = BufReader::new(file);
    if skip_wav_header(&mut reader).is_err() {
        return;
    }
    let mut bytes = vec![0; 4096];
    let mut samples = Vec::with_capacity(bytes.len() / 4);
    // Bytes of a sample split between reads.
    let mut start = 0;
    loop {
        let read = match reader.read(&mut bytes[start..]) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let end = start + read;
        let whole = end / 4 * 4;
        samples.clear();
        samples.extend(bytes[..whole].chunks_exact(4).map(|sample| {
            [
                i16::from_le_bytes([sample[0], sample[1]]),
                i16::from_le_bytes([sample[2], sample[3]]),
            ]
        }));
        let mut queue = lock(waiting);
        queue.extend(&samples);
        let excess = queue.len().saturating_sub(MAX_WAITING);
        queue.drain(..excess);
        drop(queue);
        tap.hand_on(&samples);
        bytes.copy_within(whole..end, 0);
        start = end - whole;
    }
}

/// Reads a WAV file's header from `reader`, up to its samples, and checks
/// they are 16-bit stereo at [`SAMPLE_RATE`].
fn skip_wav_header(reader: &mut impl Read) -> io::Result<()> {
    let wrong = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let mut riff = [0; 12];
    reader.read_exact(&mut riff)?;
    if &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
        return Err(wrong("the game's sound is not a WAV stream"));
    }
    loop {
        let mut chunk = [0; 8];
        reader.read_exact(&mut chunk)?;
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        match &chunk[..4] {
            b"data" => return Ok(()),
            b"fmt " => {
                let mut format = vec![0; size as usize];
                reader.read_exact(&mut format)?;
                let word = |at: usize| u16::from_le_bytes([format[at], format[at + 1]]);
                let rate = || u32::from_le_bytes([format[4], format[5], format[6], format[7]]);
                if format.len() < 16 || word(2) != 2 || rate() != SAMPLE_RATE || word(14) != 16 {
                    return Err(wrong("the game's sound is not 16-bit stereo at 48 kHz"));
                }
            }
            // Chunks are padded to an even size.
            _ => {
                io::copy(
                    &mut reader.take(u64::from(size + size % 2)),
                    &mut io::sink(),
                )?;
            }
        }
    }
}

/// What `mutex` guards, even if a thread panicked while holding it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// A WAV header as OpenAL Soft's writer starts its file: the extensible
    /// format, then the samples with their size left open.
    fn wav_header(channels: u16, bits: u16) -> Vec<u8> {
        let mut format = Vec::new();
        format.extend(0xFFFE_u16.to_le_bytes());
        format.extend(channels.to_le_bytes());
        format.extend(SAMPLE_RATE.to_le_bytes());
        format.extend((SAMPLE_RATE * 4).to_le_bytes());
        format.extend((channels * bits / 8).to_le_bytes());
        format.extend(bits.to_le_bytes());
        format.extend([0; 24]);
        let mut header = b"RIFF\xff\xff\xff\xffWAVE".to_vec();
        header.extend(b"fmt ");
        header.extend((format.len() as u32).to_le_bytes());
        header.extend(format);
        header.extend(b"data\xff\xff\xff\xff");
        header
    }

    #[test]
    fn the_header_is_read_up_to_the_samples() {
        let mut stream = wav_header(2, 16);
        stream.extend([1, 2]);
        let mut reader = stream.as_slice();
        skip_wav_header(&mut reader).expect("16-bit stereo");
        assert_eq!(reader, [1, 2]);
        assert!(skip_wav_header(&mut wav_header(1, 16).as_slice()).is_err());
        assert!(skip_wav_header(&mut wav_header(2, 32).as_slice()).is_err());
        assert!(skip_wav_header(&mut b"not a wav file".as_slice()).is_err());
    }

    #[test]
    fn sound_written_to_the_pipe_comes_out_as_samples() {
        let dir = crate::tests::temp_dir("sound");
        let pipe = dir.join("sound.fifo");
        let capture = Capture::start(&pipe, &dir.join("alsoft.conf")).expect("started");
        let settings = fs::read_to_string(dir.join("alsoft.conf")).expect("settings");
        assert!(settings.contains("drivers = wave"), "{settings}");
        assert!(
            settings.contains(&format!("file = {}", pipe.display())),
            "{settings}"
        );
        let mut player = OpenOptions::new().write(true).open(&pipe).expect("pipe");
        player.write_all(&wav_header(2, 16)).expect("header");
        // The second sample arrives in two writes.
        player
            .write_all(&[1, 0, 0xFF, 0xFF, 2, 0])
            .expect("samples");
        player.write_all(&[3, 0]).expect("the rest");
        drop(player);
        let mut out = Vec::new();
        for _ in 0..200 {
            capture.drain(&mut out);
            if out.len() == 2 {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(out, [[1, -1], [2, 3]]);
        drop(capture);
        assert!(!pipe.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_tap_hears_the_sound_as_it_comes_and_the_frames_still_get_it() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = crate::tests::temp_dir("tap");
        let pipe = dir.join("sound.fifo");
        let capture = Capture::start(&pipe, &dir.join("alsoft.conf")).expect("started");
        let (heard, hearing) = mpsc::channel();
        capture.tap().listen(Box::new(move |samples| {
            heard.send(samples.to_vec()).expect("sent")
        }));
        let mut player = OpenOptions::new().write(true).open(&pipe).expect("pipe");
        player.write_all(&wav_header(2, 16)).expect("header");
        player.write_all(&[1, 0, 0xFF, 0xFF]).expect("a sample");
        // Heard before the player writes more, or closes the pipe.
        let first = hearing.recv_timeout(Duration::from_secs(5));
        assert_eq!(first, Ok(vec![[1, -1]]));
        player.write_all(&[2, 0, 3, 0]).expect("another");
        assert_eq!(
            hearing.recv_timeout(Duration::from_secs(5)),
            Ok(vec![[2, 3]])
        );
        let mut out = Vec::new();
        capture.drain(&mut out);
        assert_eq!(out, [[1, -1], [2, 3]]);
        drop(player);
        drop(capture);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stopping_before_the_player_opens_the_pipe_does_not_hang() {
        let dir = crate::tests::temp_dir("no-sound");
        let capture =
            Capture::start(&dir.join("sound.fifo"), &dir.join("alsoft.conf")).expect("started");
        drop(capture);
        fs::remove_dir_all(&dir).ok();
    }
}
