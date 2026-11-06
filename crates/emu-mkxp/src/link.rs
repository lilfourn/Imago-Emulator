//! How ds-emulator and the bridge script in the player talk: over a Unix
//! socket on macOS and Linux, and over TCP on the loopback on Windows, where
//! the players' Ruby (3.1) has no Unix sockets. Little-endian throughout.
//!
//! 0. Over TCP, which any program on the computer can reach, the bridge
//!    first sends the token that ds-emulator gave the player in
//!    `DS_EMULATOR_TOKEN`: [`TOKEN_LEN`] ASCII hex digits. ds-emulator drops
//!    a connection that sends anything else. A Unix socket's file is the
//!    user's own, so there it sends none.
//! 1. The bridge connects and says hello: `DSEM`, the protocol
//!    [`VERSION`] (u16), the screen's width and height and the game's frame
//!    rate (u16 each), the player's process ID (u32), and the folder mkxp-z
//!    keeps the game's own files in, as its saves: its size (u16) and path,
//!    in UTF-8, or 0 if the player cannot say.
//! 2. It sends each frame once drawn: width × height pixels, each four
//!    bytes in R, G, B, A order, top row first.
//! 3. Between frames, ds-emulator sends messages of four bytes: a kind
//!    (u8), flags (u8) and buttons (u16, as [`emu_core::Buttons`] has
//!    them). The game waits for a [`STEP`], so ds-emulator paces it:
//!    - [`STEP`] runs the next frame with the buttons held, and the game
//!      sends it; its [`RESET`] flag restarts the game first.
//!    - [`SAVE`] asks for a save state. The bridge answers with its size
//!      (u32) and bytes, or [`NO_STATE`] (u32), the size of why not (u32)
//!      and why, in UTF-8.
//!    - [`LOAD`] is followed by a state's size (u32) and bytes. The next
//!      [`STEP`] restarts the game from it, as its [`RESET`] flag would.
//!
//! Either side closing the socket ends the game.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// The protocol's version, which the bridge says in its hello.
pub const VERSION: u16 = 3;
/// The message that runs the next frame.
pub const STEP: u8 = 0;
/// The message that asks for a save state.
pub const SAVE: u8 = 1;
/// The message that loads a save state.
pub const LOAD: u8 = 2;
/// [`STEP`]'s flag that restarts the game.
pub const RESET: u8 = 1;
/// The size the bridge gives instead of a state's, when it has none.
pub const NO_STATE: u32 = u32::MAX;
/// The largest state taken: far more than any game's save.
const MAX_STATE: u32 = 256 << 20;
/// What a hello starts with.
const MAGIC: &[u8; 4] = b"DSEM";
/// How many hex digits a TCP link's token has: 128 random bits.
pub const TOKEN_LEN: usize = 32;
/// How long a connection to a TCP link has to send its token.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(5);

/// What the bridge says first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// The screen's width, in pixels.
    pub width: u16,
    /// The screen's height, in pixels.
    pub height: u16,
    /// The game's frames per second.
    pub frame_rate: u16,
    /// The player's process ID.
    pub pid: u32,
    /// The folder mkxp-z keeps the game's own files in, such as its saves
    /// (`System.data_directory`), if the player said.
    pub save_folder: Option<PathBuf>,
}

/// Where the bridge connects, listening.
#[derive(Debug)]
pub enum Listener {
    /// A Unix socket, at its path.
    #[cfg(unix)]
    Unix(UnixListener, PathBuf),
    /// A TCP port on the loopback, and the token a connection must send.
    Tcp(TcpListener, String),
}

impl Listener {
    /// What the player is told, in its environment, so the bridge can
    /// connect: `DS_EMULATOR_LINK`, the socket's path or `tcp:` and the
    /// address, and over TCP, `DS_EMULATOR_TOKEN`.
    ///
    /// # Errors
    ///
    /// Fails if the TCP port cannot be read back.
    pub fn environment(&self) -> io::Result<Vec<(&'static str, String)>> {
        Ok(match self {
            #[cfg(unix)]
            Self::Unix(_, path) => vec![("DS_EMULATOR_LINK", path.display().to_string())],
            Self::Tcp(listener, token) => vec![
                (
                    "DS_EMULATOR_LINK",
                    format!("tcp:{}", listener.local_addr()?),
                ),
                ("DS_EMULATOR_TOKEN", token.clone()),
            ],
        })
    }
}

/// Listens for the bridge: at `path`, replacing whatever is there, on macOS
/// and Linux, or on Windows at a TCP port of the loopback that the system
/// picks (see [`listen_tcp`]).
///
/// # Errors
///
/// Fails if the socket cannot be made.
pub fn listen(path: &Path) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Listener::Unix(listener, path.to_owned()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        listen_tcp()
    }
}

/// Listens for the bridge at a TCP port of the loopback that the system
/// picks, with a new random token.
///
/// # Errors
///
/// Fails if the port cannot be had, or the system gives no random bytes.
pub fn listen_tcp() -> io::Result<Listener> {
    let mut random = [0; TOKEN_LEN / 2];
    getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let token = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    Ok(Listener::Tcp(listener, token))
}

/// Waits up to `timeout` for the bridge to connect to `listener`. Over TCP,
/// a connection that does not send the token first is dropped, and the
/// wait goes on.
///
/// # Errors
///
/// Fails with [`io::ErrorKind::TimedOut`] if it does not.
pub fn accept(listener: &Listener, timeout: Duration) -> io::Result<Stream> {
    accept_while(listener, timeout, || true)
}

/// Waits as [`accept`] does, but only while `waiting` says there is still
/// a bridge to wait for, as while the player runs.
///
/// # Errors
///
/// Fails with [`io::ErrorKind::TimedOut`] if the bridge does not connect,
/// or [`io::ErrorKind::UnexpectedEof`] once `waiting` says it never will.
pub fn accept_while(
    listener: &Listener,
    timeout: Duration,
    waiting: impl Fn() -> bool,
) -> io::Result<Stream> {
    let deadline = Instant::now() + timeout;
    loop {
        let accepted = match listener {
            #[cfg(unix)]
            Listener::Unix(listener, _) => listener
                .accept()
                .and_then(|(stream, _)| {
                    stream.set_nonblocking(false)?;
                    Ok(Stream::Unix(stream))
                })
                .map(Some),
            Listener::Tcp(listener, token) => listener.accept().and_then(|(stream, _)| {
                stream.set_nonblocking(false)?;
                stream.set_nodelay(true)?;
                Ok(sent_token(&stream, token).then_some(Stream::Tcp(stream)))
            }),
        };
        match accepted {
            Ok(Some(stream)) => return Ok(stream),
            Ok(None) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if !waiting() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `stream`, just accepted, sends `token` before it gives up.
fn sent_token(mut stream: &TcpStream, token: &str) -> bool {
    let mut sent = [0; TOKEN_LEN];
    let read = stream
        .set_read_timeout(Some(TOKEN_TIMEOUT))
        .and_then(|()| stream.read_exact(&mut sent))
        .and_then(|()| stream.set_read_timeout(None));
    // Every byte is compared, so how long it takes says nothing of where
    // a guess went wrong.
    let differs = sent
        .iter()
        .zip(token.as_bytes())
        .fold(0, |differs, (a, b)| differs | (a ^ b));
    read.is_ok() && token.len() == TOKEN_LEN && differs == 0
}

/// A connection to the bridge.
#[derive(Debug)]
pub enum Stream {
    /// Over a Unix socket.
    #[cfg(unix)]
    Unix(UnixStream),
    /// Over TCP.
    Tcp(TcpStream),
}

impl Stream {
    /// How long a read may wait, or `None` for as long as it takes.
    ///
    /// # Errors
    ///
    /// Fails if the system refuses it.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_read_timeout(timeout),
            Self::Tcp(stream) => stream.set_read_timeout(timeout),
        }
    }

    /// How long a write may wait, or `None` for as long as it takes.
    ///
    /// # Errors
    ///
    /// Fails if the system refuses it.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_write_timeout(timeout),
            Self::Tcp(stream) => stream.set_write_timeout(timeout),
        }
    }

    fn shutdown(&self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
            Self::Tcp(stream) => stream.shutdown(Shutdown::Both),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
            Self::Tcp(stream) => stream.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
            Self::Tcp(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
            Self::Tcp(stream) => stream.flush(),
        }
    }
}

/// ds-emulator's end of the link.
#[derive(Debug)]
pub struct Link {
    stream: Stream,
}

impl Link {
    /// The link over `stream`, connected to the bridge.
    pub fn new(stream: Stream) -> Self {
        Self { stream }
    }

    /// Reads the bridge's hello.
    ///
    /// # Errors
    ///
    /// Fails if the socket fails or the hello is not one this version
    /// reads.
    pub fn hello(&mut self) -> io::Result<Hello> {
        let mut bytes = [0; 16];
        self.stream.read_exact(&mut bytes)?;
        let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        if &bytes[..4] != MAGIC || word(4) != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the game's bridge speaks another version of the link",
            ));
        }
        if word(6) == 0 || word(8) == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the game said its screen has no size",
            ));
        }
        let mut size = [0; 2];
        self.stream.read_exact(&mut size)?;
        let mut folder = vec![0; usize::from(u16::from_le_bytes(size))];
        self.stream.read_exact(&mut folder)?;
        let save_folder = String::from_utf8(folder)
            .ok()
            .filter(|folder| !folder.is_empty())
            .map(PathBuf::from);
        Ok(Hello {
            width: word(6),
            height: word(8),
            frame_rate: word(10),
            pid: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            save_folder,
        })
    }

    /// Reads the next frame into `pixels`, which holds exactly one.
    ///
    /// # Errors
    ///
    /// Fails if the socket fails or closes first.
    pub fn frame(&mut self, pixels: &mut [u8]) -> io::Result<()> {
        self.stream.read_exact(pixels)
    }

    /// Runs the next frame with `buttons` held, and `flags`.
    ///
    /// # Errors
    ///
    /// Fails if the socket fails or is closed.
    pub fn step(&mut self, buttons: u16, flags: u8) -> io::Result<()> {
        let [low, high] = buttons.to_le_bytes();
        self.stream.write_all(&[STEP, flags, low, high])
    }

    /// Asks for a save state: its bytes, or why the game cannot be saved
    /// now.
    ///
    /// # Errors
    ///
    /// Fails if the socket fails or closes, or the answer is garbled.
    pub fn save(&mut self) -> io::Result<Result<Vec<u8>, String>> {
        self.stream.write_all(&[SAVE, 0, 0, 0])?;
        match self.word()? {
            NO_STATE => {
                let size = self.word()?;
                let why = self.bytes(size)?;
                Ok(Err(String::from_utf8_lossy(&why).into_owned()))
            }
            size => Ok(Ok(self.bytes(size)?)),
        }
    }

    /// Restarts the game from the save state `state` at the next step.
    ///
    /// # Errors
    ///
    /// Fails if the socket fails or is closed, or the state is too big.
    pub fn load(&mut self, state: &[u8]) -> io::Result<()> {
        let size = u32::try_from(state.len())
            .ok()
            .filter(|size| *size <= MAX_STATE)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the state is too big"))?;
        self.stream.write_all(&[LOAD, 0, 0, 0])?;
        self.stream.write_all(&size.to_le_bytes())?;
        self.stream.write_all(state)
    }

    fn word(&mut self) -> io::Result<u32> {
        let mut bytes = [0; 4];
        self.stream.read_exact(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn bytes(&mut self, size: u32) -> io::Result<Vec<u8>> {
        if size > MAX_STATE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the game sent a state too big to be one",
            ));
        }
        let mut bytes = vec![0; size as usize];
        self.stream.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Closes the link, which ends the game.
    pub fn close(&self) {
        self.stream.shutdown().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two ends of a connection: a Unix socket's where there are, or
    /// TCP's on the loopback.
    fn pair() -> (Stream, Stream) {
        #[cfg(unix)]
        {
            let (ours, bridge) = UnixStream::pair().expect("socket pair");
            (Stream::Unix(ours), Stream::Unix(bridge))
        }
        #[cfg(not(unix))]
        {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listening");
            let bridge =
                TcpStream::connect(listener.local_addr().expect("address")).expect("connected");
            let (ours, _) = listener.accept().expect("accepted");
            (Stream::Tcp(ours), Stream::Tcp(bridge))
        }
    }

    /// What the bridge sends first, as bridge.rb packs it.
    fn hello_bytes(width: u16, height: u16, frame_rate: u16, pid: u32) -> Vec<u8> {
        hello_with_folder(width, height, frame_rate, pid, "")
    }

    fn hello_with_folder(
        width: u16,
        height: u16,
        frame_rate: u16,
        pid: u32,
        folder: &str,
    ) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        let size = u16::try_from(folder.len()).expect("a short folder");
        for word in [VERSION, width, height, frame_rate] {
            bytes.extend(word.to_le_bytes());
        }
        bytes.extend(pid.to_le_bytes());
        bytes.extend(size.to_le_bytes());
        bytes.extend(folder.as_bytes());
        bytes
    }

    #[test]
    fn hellos_frames_and_answers_cross_the_link() {
        let (ours, mut bridge) = pair();
        let mut link = Link::new(ours);
        let folder = "/Users/me/Library/Application Support/mkxp-z/Pokémon Game/";
        bridge
            .write_all(&hello_with_folder(2, 1, 40, 1234, folder))
            .expect("hello");
        bridge.write_all(&[1, 2, 3, 4, 5, 6, 7, 8]).expect("frame");
        assert_eq!(
            link.hello().expect("read"),
            Hello {
                width: 2,
                height: 1,
                frame_rate: 40,
                pid: 1234,
                save_folder: Some(PathBuf::from(folder)),
            }
        );
        let mut pixels = [0; 8];
        link.frame(&mut pixels).expect("frame read");
        assert_eq!(pixels, [1, 2, 3, 4, 5, 6, 7, 8]);
        link.step(0x0801, RESET).expect("answer");
        let mut answer = [0; 4];
        bridge.read_exact(&mut answer).expect("answer read");
        assert_eq!(answer, [STEP, RESET, 0x01, 0x08]);
        drop(bridge);
        assert_eq!(
            link.frame(&mut pixels).map_err(|error| error.kind()),
            Err(io::ErrorKind::UnexpectedEof)
        );
    }

    #[test]
    fn states_go_both_ways_and_a_refusal_says_why() {
        let (ours, mut bridge) = pair();
        let mut link = Link::new(ours);
        let mut answer = |bytes: &[u8]| bridge.write_all(bytes).expect("answer");
        answer(&[3, 0, 0, 0, b'a', b'b', b'c']);
        let why = b"in battle";
        let mut refusal = NO_STATE.to_le_bytes().to_vec();
        refusal.extend((why.len() as u32).to_le_bytes());
        refusal.extend(why);
        answer(&refusal);
        assert_eq!(link.save().expect("saved"), Ok(b"abc".to_vec()));
        assert_eq!(link.save().expect("answered"), Err("in battle".to_owned()));
        link.load(b"xyz").expect("loaded");
        let mut asked = [0; 4 + 4 + 4 + 4 + 3];
        bridge.read_exact(&mut asked).expect("messages");
        assert_eq!(asked[..8], [SAVE, 0, 0, 0, SAVE, 0, 0, 0]);
        assert_eq!(asked[8..], [LOAD, 0, 0, 0, 3, 0, 0, 0, b'x', b'y', b'z']);
    }

    #[test]
    fn a_player_that_cannot_say_where_it_saves_has_no_save_folder() {
        let (ours, mut bridge) = pair();
        let mut link = Link::new(ours);
        bridge
            .write_all(&hello_bytes(640, 480, 40, 7))
            .expect("hello");
        assert_eq!(link.hello().expect("read").save_folder, None);
    }

    #[test]
    fn another_version_or_an_empty_screen_is_refused() {
        let mut old = hello_bytes(512, 384, 40, 1);
        old[4] = 9;
        for hello in [old, hello_bytes(0, 384, 40, 1)] {
            let (ours, mut bridge) = pair();
            let mut link = Link::new(ours);
            bridge.write_all(&hello).expect("hello");
            assert_eq!(
                link.hello().map_err(|error| error.kind()).err(),
                Some(io::ErrorKind::InvalidData)
            );
        }
    }

    /// Connects to `listener` as the bridge does, from what the player is
    /// told, sending `token` over TCP: by default the right one.
    fn connect(listener: &Listener, token: Option<&str>) -> Box<dyn Write> {
        let environment = listener.environment().expect("environment");
        let link = &environment[0].1;
        match link.strip_prefix("tcp:") {
            Some(address) => {
                let mut stream = TcpStream::connect(address).expect("connects");
                let right = environment.get(1).map(|(_, token)| token.as_str());
                stream
                    .write_all(token.or(right).expect("a token").as_bytes())
                    .expect("token");
                Box::new(stream)
            }
            #[cfg(unix)]
            None => Box::new(UnixStream::connect(link).expect("connects")),
            #[cfg(not(unix))]
            None => panic!("no Unix sockets here: {link}"),
        }
    }

    #[test]
    fn accepting_gives_up_after_the_timeout() {
        let dir = crate::tests::temp_dir("accept");
        for listener in [
            listen(&dir.join("link.sock")).expect("listening"),
            listen_tcp().expect("listening on TCP"),
        ] {
            let waited = Instant::now();
            assert_eq!(
                accept(&listener, Duration::from_millis(50))
                    .map_err(|error| error.kind())
                    .err(),
                Some(io::ErrorKind::TimedOut)
            );
            assert!(waited.elapsed() >= Duration::from_millis(50));
            let _bridge = connect(&listener, None);
            assert!(accept(&listener, Duration::from_secs(5)).is_ok());
            assert_eq!(
                accept_while(&listener, Duration::from_secs(5), || false)
                    .map_err(|error| error.kind())
                    .err(),
                Some(io::ErrorKind::UnexpectedEof),
                "a player that has gone is not waited for"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn over_tcp_only_the_bridge_with_the_token_gets_in() {
        let listener = listen_tcp().expect("listening");
        let environment = listener.environment().expect("environment");
        assert!(
            environment[0].1.starts_with("tcp:127.0.0.1:"),
            "{environment:?}"
        );
        let token = &environment[1].1;
        assert_eq!(token.len(), TOKEN_LEN);
        assert!(token.bytes().all(|digit| digit.is_ascii_hexdigit()));
        let other = listen_tcp().expect("another");
        assert_ne!(&other.environment().expect("environment")[1].1, token);
        // A program that guesses wrong, then one that sends nothing and
        // leaves, are dropped; the bridge that follows gets in.
        let _wrong = connect(&listener, Some(&"0".repeat(TOKEN_LEN)));
        drop(connect(&listener, Some("")));
        let mut bridge = connect(&listener, None);
        let mut stream = accept(&listener, Duration::from_secs(5)).expect("the bridge");
        bridge.write_all(b"DSEM").expect("sent");
        let mut first = [0; 4];
        stream.read_exact(&mut first).expect("read");
        assert_eq!(&first, b"DSEM");
    }
}
