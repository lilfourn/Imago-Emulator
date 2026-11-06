//! How the app and the 3DS helper talk: over the helper's stdin and
//! stdout, little-endian throughout. Each message is a kind (u8), its
//! payload's length (u32) and the payload.
//!
//! The app asks and the helper answers each request once, in order:
//!
//! | Request | Payload | Answer |
//! |---|---|---|
//! | [`RUN`] | buttons (u16), circle pad x, y (i16 each), touching (u8), touch x, y (u16 each) | [`FRAME`] |
//! | [`SAVE`] | none | [`STATE`] or [`REFUSED`] |
//! | [`LOAD`] | a state | [`DONE`] or [`REFUSED`] |
//! | [`RESET`] | none | [`DONE`] |
//! | [`SCALE`] | the internal resolution, 1 to 10 (u32) | [`DONE`] |
//! | [`TEXTURES`] | the core's texture filter, in UTF-8: `none`, `ScaleForce` or `MMPX` | [`DONE`] |
//! | [`QUIT`] | none | none: the helper exits |
//!
//! The helper starts by saying [`HELLO`]: the protocol's [`VERSION`]
//! (u16) and the renderer's name. A [`FRAME`] is the top screen's width and
//! height and the bottom screen's (u16 each), the number of stereo samples
//! (u32), then the top screen's pixels and the bottom's (u32 each, as
//! [`Frame`] has them) and the samples (i16 left, i16 right). [`FAILED`],
//! said in place of any answer, is why the helper stopped, in UTF-8; it
//! exits after.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use emu_core::{Frame, ScreenSize, StereoSample};

use crate::{HELPER_FLAG, OpenError, install::Parts};

/// The protocol's version, which the helper says in its hello.
pub const VERSION: u16 = 2;

/// Runs a frame with the pad held.
pub const RUN: u8 = 1;
/// Asks for a save state.
pub const SAVE: u8 = 2;
/// Loads a save state.
pub const LOAD: u8 = 3;
/// Power-cycles the console.
pub const RESET: u8 = 4;
/// Sets the internal resolution.
pub const SCALE: u8 = 5;
/// Asks the helper to exit.
pub const QUIT: u8 = 6;
/// Sets the texture filter.
pub const TEXTURES: u8 = 7;

/// The helper is ready.
pub const HELLO: u8 = 1;
/// A frame's screens and sound.
pub const FRAME: u8 = 2;
/// A save state.
pub const STATE: u8 = 3;
/// A request without an answer of its own is done.
pub const DONE: u8 = 4;
/// A save or load could not be done, and why; the game plays on.
pub const REFUSED: u8 = 5;
/// The helper stopped, and why.
pub const FAILED: u8 = 6;

/// How long the helper may take to load the game and say hello: a first
/// start builds the core's caches.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
/// How long one answer may take. A save state waits up to five seconds
/// for the game's file work, so this is well beyond any frame or state.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the helper has to exit once asked, before it is killed.
const QUIT_GRACE: Duration = Duration::from_secs(2);
/// The largest message either side accepts: a frame at 10× is under 70 MB,
/// and a state of a New 3DS's 256 MB of memory fits too.
const MAX_MESSAGE: u32 = 512 << 20;

/// What the player holds during a frame, in libretro's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pad {
    /// libretro joypad bits.
    pub buttons: u16,
    /// The circle pad's tilt on each axis, up and left negative.
    pub circle: (i16, i16),
    /// Where the bottom screen is touched, in native pixels.
    pub touch: Option<(u16, u16)>,
}

impl Pad {
    /// The [`RUN`] payload.
    fn encode(self) -> [u8; 11] {
        let mut out = [0; 11];
        out[0..2].copy_from_slice(&self.buttons.to_le_bytes());
        out[2..4].copy_from_slice(&self.circle.0.to_le_bytes());
        out[4..6].copy_from_slice(&self.circle.1.to_le_bytes());
        let (touching, (x, y)) = match self.touch {
            Some(point) => (1, point),
            None => (0, (0, 0)),
        };
        out[6] = touching;
        out[7..9].copy_from_slice(&x.to_le_bytes());
        out[9..11].copy_from_slice(&y.to_le_bytes());
        out
    }

    /// Reads a [`RUN`] payload.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let bytes: &[u8; 11] = payload.try_into().ok()?;
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let i16_at = |at: usize| i16::from_le_bytes([bytes[at], bytes[at + 1]]);
        Some(Self {
            buttons: u16_at(0),
            circle: (i16_at(2), i16_at(4)),
            touch: (bytes[6] != 0).then(|| (u16_at(7), u16_at(9))),
        })
    }
}

/// Writes one message.
pub fn write_message(out: &mut impl Write, kind: u8, parts: &[&[u8]]) -> io::Result<()> {
    let length: usize = parts.iter().map(|part| part.len()).sum();
    let length = u32::try_from(length)
        .ok()
        .filter(|length| *length <= MAX_MESSAGE)
        .ok_or_else(|| io::Error::other("message too large"))?;
    out.write_all(&[kind])?;
    out.write_all(&length.to_le_bytes())?;
    for part in parts {
        out.write_all(part)?;
    }
    out.flush()
}

/// Reads one message into `payload`, returning its kind, or `None` at the
/// end of the stream.
pub fn read_message(input: &mut impl Read, payload: &mut Vec<u8>) -> io::Result<Option<u8>> {
    let mut head = [0; 5];
    match input.read_exact(&mut head[..1]) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    input.read_exact(&mut head[1..])?;
    let length = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    if length > MAX_MESSAGE {
        return Err(io::Error::other(format!("a message of {length} bytes")));
    }
    // A buffer handed back from an earlier message is reused as it is: the
    // read fills every byte, so only growth needs zeroing.
    payload.truncate(length as usize);
    payload.resize(length as usize, 0);
    input.read_exact(payload)?;
    Ok(Some(head[0]))
}

/// The head of a [`FRAME`]: the two screens' sizes and the samples' count.
pub fn frame_head(top: ScreenSize, bottom: ScreenSize, samples: usize) -> [u8; 12] {
    let mut out = [0; 12];
    out[0..2].copy_from_slice(&top.width.to_le_bytes());
    out[2..4].copy_from_slice(&top.height.to_le_bytes());
    out[4..6].copy_from_slice(&bottom.width.to_le_bytes());
    out[6..8].copy_from_slice(&bottom.height.to_le_bytes());
    out[8..12].copy_from_slice(&u32::try_from(samples).unwrap_or(u32::MAX).to_le_bytes());
    out
}

/// Copies a [`FRAME`] payload into `screens` (resized if its sizes
/// changed) and appends its samples to `audio`.
fn decode_frame(
    payload: &[u8],
    screens: &mut [Frame; 2],
    audio: &mut Vec<StereoSample>,
) -> Result<(), String> {
    let bad = || "the 3DS helper sent a broken frame".to_owned();
    let head = payload.get(..12).ok_or_else(bad)?;
    let u16_at = |at: usize| u16::from_le_bytes([head[at], head[at + 1]]);
    let sizes = [
        ScreenSize::new(u16_at(0), u16_at(2)),
        ScreenSize::new(u16_at(4), u16_at(6)),
    ];
    let samples = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
    let mut rest = &payload[12..];
    for (screen, size) in screens.iter_mut().zip(sizes) {
        if screen.size() != size {
            *screen = Frame::new(size);
        }
        let bytes = rest.get(..size.pixel_count() * 4).ok_or_else(bad)?;
        for (pixel, word) in screen.pixels_mut().iter_mut().zip(bytes.chunks_exact(4)) {
            *pixel = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        rest = &rest[bytes.len()..];
    }
    let sound = rest.get(..samples * 4).ok_or_else(bad)?;
    audio.extend(sound.chunks_exact(4).map(|sample| -> StereoSample {
        [
            i16::from_le_bytes([sample[0], sample[1]]),
            i16::from_le_bytes([sample[2], sample[3]]),
        ]
    }));
    Ok(())
}

/// An answer from the helper, read on a thread of its own so that waiting
/// for one can time out.
enum Answer {
    /// A message: its kind and payload.
    Message(u8, Vec<u8>),
    /// The helper closed its end, or reading failed, and why.
    Gone(String),
}

/// The app's end of the link: the helper process and its pipes.
pub struct Client {
    child: Option<Child>,
    to_helper: BufWriter<ChildStdin>,
    answers: Receiver<Answer>,
    /// Frames' buffers, handed back to the reading thread to fill again,
    /// so a frame of several megabytes is not allocated each time.
    spare: Sender<Vec<u8>>,
    /// Where the helper's own messages go, named when it stops.
    log: PathBuf,
    /// Why the helper stopped, once it has: every request after fails with
    /// it, as an answer that came late would belong to the wrong request.
    dead: Option<String>,
}

impl Client {
    /// Starts the helper on `game` and waits for its hello.
    pub fn start(
        helper: &Path,
        game: &Path,
        data: &Path,
        parts: &Parts,
    ) -> Result<Self, OpenError> {
        let start = |why: io::Error| OpenError::Start(why.to_string());
        std::fs::create_dir_all(data).map_err(start)?;
        let log = data.join("helper.log");
        // The last start's log stays beside it, for when that one stopped
        // and the player tries again.
        let _ = std::fs::rename(&log, data.join("helper.previous.log"));
        let stderr = File::create(&log).map_err(start)?;
        let mut command = Command::new(helper);
        command
            .arg(HELPER_FLAG)
            .arg(&parts.core)
            .arg(&parts.moltenvk)
            .arg(game)
            .arg(data)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        if std::env::var_os("MVK_CONFIG_LOG_LEVEL").is_none() {
            // MoltenVK's errors only: its notes on every pipeline would
            // fill the log.
            command.env("MVK_CONFIG_LOG_LEVEL", "1");
        }
        let mut child = command.spawn().map_err(start)?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(OpenError::Start("its pipes were not made".to_owned()));
        };
        let (sender, answers) = mpsc::channel();
        let (spare, spares) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("3DS helper link".to_owned())
            .spawn(move || {
                // The emulation thread waits on it for every frame.
                crate::host::interactive_thread();
                let mut from_helper = BufReader::with_capacity(1 << 20, stdout);
                loop {
                    let mut payload = spares.try_recv().unwrap_or_default();
                    let answer = match read_message(&mut from_helper, &mut payload) {
                        Ok(Some(kind)) => Answer::Message(kind, payload),
                        Ok(None) => Answer::Gone("it closed the link".to_owned()),
                        Err(error) => Answer::Gone(error.to_string()),
                    };
                    let gone = matches!(answer, Answer::Gone(_));
                    if sender.send(answer).is_err() || gone {
                        break;
                    }
                }
            })
            .map_err(start)?;
        let mut client = Self {
            child: Some(child),
            to_helper: BufWriter::with_capacity(64 << 10, stdin),
            answers,
            spare,
            log,
            dead: None,
        };
        match client.answer(STARTUP_TIMEOUT) {
            Ok((HELLO, payload)) if payload.get(..2) == Some(&VERSION.to_le_bytes()[..]) => {
                Ok(client)
            }
            Ok((HELLO, _)) => Err(OpenError::Start(
                "it speaks another version of the link".to_owned(),
            )),
            Ok((kind, _)) => Err(OpenError::Start(format!("it said {kind} for hello"))),
            Err(Stopped::Failed(why)) => Err(OpenError::Game(why)),
            Err(Stopped::Gone(why)) => Err(OpenError::Start(why)),
        }
    }

    /// Runs one frame with `pad` held, copying its screens into `screens`
    /// and appending its sound to `audio`.
    pub fn run(
        &mut self,
        pad: Pad,
        screens: &mut [Frame; 2],
        audio: &mut Vec<StereoSample>,
    ) -> Result<(), String> {
        let payload = self.ask(RUN, &pad.encode(), FRAME)?;
        let decoded = decode_frame(&payload, screens, audio);
        // The reading thread has gone if this fails, which the next
        // request finds out.
        let _ = self.spare.send(payload);
        decoded.map_err(|why| self.ended(Stopped::Gone(why)))
    }

    /// Why the helper stopped, once it has.
    pub fn dead(&self) -> Option<&str> {
        self.dead.as_deref()
    }

    /// Asks for a save state, which replaces `out`'s contents.
    pub fn save_state(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        self.send(SAVE, &[])?;
        match self
            .answer(ANSWER_TIMEOUT)
            .map_err(|stopped| self.ended(stopped))?
        {
            (STATE, state) => {
                out.clear();
                out.extend_from_slice(&state);
                Ok(())
            }
            (REFUSED, why) => Err(String::from_utf8_lossy(&why).into_owned()),
            (kind, _) => Err(self.ended(Stopped::Gone(format!("it said {kind} for a state")))),
        }
    }

    /// Loads `state`.
    pub fn load_state(&mut self, state: &[u8]) -> Result<(), String> {
        self.send(LOAD, state)?;
        match self
            .answer(ANSWER_TIMEOUT)
            .map_err(|stopped| self.ended(stopped))?
        {
            (DONE, _) => Ok(()),
            (REFUSED, why) => Err(String::from_utf8_lossy(&why).into_owned()),
            (kind, _) => Err(self.ended(Stopped::Gone(format!("it said {kind} for a load")))),
        }
    }

    /// Power-cycles the console.
    pub fn reset(&mut self) -> Result<(), String> {
        self.ask(RESET, &[], DONE).map(drop)
    }

    /// Sets the internal resolution to `scale` times native.
    pub fn set_scale(&mut self, scale: u32) -> Result<(), String> {
        self.ask(SCALE, &scale.to_le_bytes(), DONE).map(drop)
    }

    /// Sets the core's texture filter, by its name in the core's options.
    pub fn set_texture_filter(&mut self, filter: &str) -> Result<(), String> {
        self.ask(TEXTURES, filter.as_bytes(), DONE).map(drop)
    }

    /// Sends a request and waits for its answer, which must be `expected`.
    fn ask(&mut self, kind: u8, payload: &[u8], expected: u8) -> Result<Vec<u8>, String> {
        self.send(kind, payload)?;
        match self.answer(ANSWER_TIMEOUT) {
            Ok((answer, payload)) if answer == expected => Ok(payload),
            Ok((answer, _)) => Err(self.ended(Stopped::Gone(format!(
                "it said {answer} where {expected} was due"
            )))),
            Err(stopped) => Err(self.ended(stopped)),
        }
    }

    fn send(&mut self, kind: u8, payload: &[u8]) -> Result<(), String> {
        if let Some(why) = &self.dead {
            return Err(why.clone());
        }
        write_message(&mut self.to_helper, kind, &[payload])
            .map_err(|error| self.ended(Stopped::Gone(error.to_string())))
    }

    /// The next answer, or why there is none.
    fn answer(&mut self, timeout: Duration) -> Result<(u8, Vec<u8>), Stopped> {
        match self.answers.recv_timeout(timeout) {
            Ok(Answer::Message(FAILED, why)) => {
                Err(Stopped::Failed(String::from_utf8_lossy(&why).into_owned()))
            }
            Ok(Answer::Message(kind, payload)) => Ok((kind, payload)),
            Ok(Answer::Gone(why)) => Err(Stopped::Gone(why)),
            Err(RecvTimeoutError::Timeout) => Err(Stopped::Gone(format!(
                "it did not answer for {} seconds",
                timeout.as_secs()
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(Stopped::Gone("its link closed".to_owned())),
        }
    }

    /// Notes that the helper has stopped, and returns why in words for
    /// the player.
    fn ended(&mut self, stopped: Stopped) -> String {
        let why = match stopped {
            Stopped::Failed(why) => why,
            Stopped::Gone(why) => format!(
                "The 3DS core stopped ({why}). Its log is {}.",
                self.log.display()
            ),
        };
        self.dead.get_or_insert(why).clone()
    }
}

/// Why no answer came.
enum Stopped {
    /// The helper said why it stopped.
    Failed(String),
    /// It went without a word.
    Gone(String),
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = write_message(&mut self.to_helper, QUIT, &[]);
        let Some(mut child) = self.child.take() else {
            return;
        };
        // Give the helper a moment to save its files and exit, without
        // holding up whoever dropped the game.
        let _ = thread::Builder::new()
            .name("3DS helper exit".to_owned())
            .spawn(move || {
                let deadline = Instant::now() + QUIT_GRACE;
                while Instant::now() < deadline {
                    if !matches!(child.try_wait(), Ok(None)) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::rgb;

    /// A helper that says hello in this protocol's version, then exits: a
    /// shell script, so on Unix only.
    #[cfg(unix)]
    fn helper_that_dies(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let [low, high] = VERSION.to_le_bytes();
        let script = dir.join("helper.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '\\001\\002\\000\\000\\000\\{low:03o}\\{high:03o}'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    #[cfg(unix)]
    fn once_the_helper_has_gone_every_request_fails_with_why() {
        let dir = std::env::temp_dir().join(format!("emu-3ds-link-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("helper.log"), "the last start").unwrap();
        let parts = Parts {
            core: dir.join("core.dylib"),
            moltenvk: dir.join("moltenvk.dylib"),
        };
        let mut client =
            Client::start(&helper_that_dies(&dir), &dir.join("game.3ds"), &dir, &parts)
                .expect("the helper said hello");
        let mut screens = crate::SCREENS.map(Frame::new);
        let why = client
            .run(Pad::default(), &mut screens, &mut Vec::new())
            .expect_err("the helper has gone");
        assert!(why.starts_with("The 3DS core stopped"), "{why}");
        assert_eq!(client.dead(), Some(why.as_str()));
        assert_eq!(
            client.reset(),
            Err(why.clone()),
            "no request goes out after"
        );
        assert_eq!(client.save_state(&mut Vec::new()), Err(why));
        assert_eq!(
            std::fs::read_to_string(dir.join("helper.previous.log")).unwrap(),
            "the last start",
            "the last start's log is kept"
        );
        drop(client);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pads_round_trip() {
        for pad in [
            Pad::default(),
            Pad {
                buttons: 0x3FFF,
                circle: (-i16::MAX, 23_170),
                touch: Some((319, 239)),
            },
        ] {
            assert_eq!(Pad::decode(&pad.encode()), Some(pad));
        }
        assert_eq!(Pad::decode(&[0; 3]), None);
    }

    #[test]
    fn messages_round_trip_and_end_cleanly() {
        let mut wire = Vec::new();
        write_message(&mut wire, STATE, &[b"ab", b"cd"]).unwrap();
        write_message(&mut wire, DONE, &[]).unwrap();
        let mut input = wire.as_slice();
        let mut payload = Vec::new();
        assert_eq!(read_message(&mut input, &mut payload).unwrap(), Some(STATE));
        assert_eq!(payload, b"abcd");
        assert_eq!(read_message(&mut input, &mut payload).unwrap(), Some(DONE));
        assert!(payload.is_empty());
        assert_eq!(read_message(&mut input, &mut payload).unwrap(), None);
    }

    #[test]
    fn a_frame_fills_both_screens_and_the_sound() {
        let top = ScreenSize::new(2, 1);
        let bottom = ScreenSize::new(1, 2);
        let pixels: Vec<u8> = [rgb(1, 2, 3), rgb(4, 5, 6), rgb(7, 8, 9), rgb(10, 11, 12)]
            .iter()
            .flat_map(|pixel| pixel.to_le_bytes())
            .collect();
        let sound: Vec<u8> = [-1i16, 2, 3, -4]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let payload = [&frame_head(top, bottom, 2)[..], &pixels, &sound].concat();
        let mut screens = crate::SCREENS.map(Frame::new);
        let mut audio = Vec::new();
        decode_frame(&payload, &mut screens, &mut audio).unwrap();
        assert_eq!(screens[0].size(), top);
        assert_eq!(screens[0].pixels(), &[rgb(1, 2, 3), rgb(4, 5, 6)]);
        assert_eq!(screens[1].pixels(), &[rgb(7, 8, 9), rgb(10, 11, 12)]);
        assert_eq!(audio, [[-1, 2], [3, -4]]);
        assert!(decode_frame(&payload[..20], &mut screens, &mut audio).is_err());
    }
}
