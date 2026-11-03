//! The helper process: Azahar's core, driven a frame at a time over the
//! link (see [`crate::link`]).
//!
//! libretro calls back into the frontend through plain C functions with
//! no context, so what they share with the loop lives in one static,
//! [`SHARED`], locked only between calls into the core, never across one.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use emu_core::ScreenSize;

use crate::libretro::{self as lr, Api};
use crate::link::{self, Pad};
use crate::vulkan::Gpu;

/// The composed picture the core draws at 1×: the top screen over the
/// bottom one, which sits centred between bars.
const PICTURE: (u32, u32) = (400, 480);
/// Where the bottom screen starts in the picture at 1×.
const BOTTOM_AT: (u32, u32) = (40, 240);
/// The highest internal resolution the helper asks for, as a multiple of
/// the native one. The core goes to 10×, but every frame is read back and
/// sent to the app: at 4× that is 11 MB a frame, and the link measured
/// about 250 frames a second there, so 4× keeps room for a heavy game at
/// full speed; 8× (44 MB a frame) would not keep 60.
const MAX_SCALE: u32 = 4;
/// How many frames a game may take to show its first.
const STARTUP_FRAMES: usize = 10;
/// Why `.3dsx` homebrew has no save states on the GPU. Azahar's core
/// rebuilds its renderer when it loads a state, and makes the new one's
/// Vulkan pipeline cache only for a game with a title ID; without one, the
/// next frame crashes (Azahar 2126.2). A state saved could never be
/// loaded, so none is made.
pub(crate) const NO_HOMEBREW_STATES: &str = "Save states don't work for this homebrew game: Azahar's core can't load them for a game without a title ID";

/// Which of the core's renderers draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Renderer {
    /// The GPU, through MoltenVK.
    Vulkan,
    /// The CPU: slow, but needs no GPU.
    Software,
}

impl Renderer {
    const fn name(self) -> &'static str {
        match self {
            Self::Vulkan => "Vulkan",
            Self::Software => "software",
        }
    }
}

/// The core's options, as its `citra_*` variables, for `renderer`. The
/// rest keep the core's defaults (a New 3DS, the region the game asks
/// for, the JIT, the shader cache on disk).
fn options(renderer: Renderer) -> HashMap<String, CString> {
    let api = match renderer {
        Renderer::Vulkan => "Vulkan",
        Renderer::Software => "Software",
    };
    [
        ("citra_graphics_api", api),
        ("citra_resolution_factor", "1"),
        // Top over bottom; the helper cuts the picture into the two
        // screens and the app lays them out as the player chose.
        ("citra_layout_option", "default"),
        ("citra_swap_screen", "Top"),
        // Touch comes as the pointer; the right stick is the C stick.
        ("citra_enable_mouse_touchscreen", "disabled"),
        ("citra_enable_touch_touchscreen", "enabled"),
        ("citra_enable_touch_pointer_timeout", "disabled"),
        ("citra_analog_function", "c_stick"),
        ("citra_use_virtual_sd", "enabled"),
        ("citra_use_libretro_save_path", "LibRetro Default"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .chain(overrides())
    .filter_map(|(key, value)| Some((key, CString::new(value).ok()?)))
    .collect()
}

/// Options from `DS_EMULATOR_3DS_OPTIONS`, as `key=value` pairs split by
/// commas, which replace the helper's own: for trying the core's settings
/// without a build.
fn overrides() -> Vec<(String, String)> {
    std::env::var("DS_EMULATOR_3DS_OPTIONS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// A raw pointer the core handed over, kept in [`SHARED`].
struct Handed<T>(*const T);

impl<T> Clone for Handed<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handed<T> {}

// SAFETY: the pointers are only dereferenced on the helper's main thread,
// during calls the core expects them to be used in, and point at memory
// the core keeps for as long as the game is loaded.
unsafe impl<T> Send for Handed<T> {}

/// The core's picture of the last frame, as [`emu_core::Frame`] pixels.
#[derive(Default)]
pub struct Picture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `0xAARRGGBB`, row-major, alpha always `0xFF`.
    pub pixels: Vec<u32>,
}

impl Picture {
    /// Makes the picture `width` × `height`, keeping its buffer.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels.resize(width as usize * height as usize, 0);
    }
}

/// What the callbacks and the loop share.
struct Shared {
    /// What the player holds this frame.
    pad: Pad,
    /// Where the pointer was last, in libretro's -32,767..32,767, kept
    /// while the screen isn't touched, as a lifted stylus stays put.
    pointer: (i16, i16),
    /// The software renderer's last frame, and whether it drew it this
    /// run rather than repeating the one before.
    picture: Picture,
    fresh: bool,
    /// The size of the hardware renderer's frame, when it drew one this
    /// run.
    gpu_frame: Option<(u32, u32)>,
    /// Whether the core drew a frame this run, rather than repeating the
    /// last or showing none, as it does while no game is loaded.
    drew: bool,
    /// This frame's sound, interleaved.
    audio: Vec<i16>,
    /// The last message the core showed, which says why it refused a game.
    message: Option<String>,
    /// The core's variables, and whether one changed since it last asked.
    options: HashMap<String, CString>,
    options_changed: bool,
    /// The folder the core keeps its files in.
    directory: CString,
    /// Which renderer the core was told to use.
    renderer: Renderer,
    /// The hardware renderer's callbacks, once the core asked for one.
    context_reset: Option<lr::HwContextResetFn>,
    context_destroy: Option<lr::HwContextResetFn>,
    negotiation: Option<Handed<lr::VulkanNegotiation>>,
    /// The Vulkan interface handed to the core.
    interface: Option<Handed<lr::VulkanInterface>>,
}

static SHARED: LazyLock<Mutex<Shared>> = LazyLock::new(|| {
    Mutex::new(Shared {
        pad: Pad::default(),
        pointer: (0, 0),
        picture: Picture::default(),
        fresh: false,
        gpu_frame: None,
        drew: false,
        audio: Vec::new(),
        message: None,
        options: HashMap::new(),
        options_changed: false,
        directory: CString::default(),
        renderer: Renderer::Software,
        context_reset: None,
        context_destroy: None,
        negotiation: None,
        interface: None,
    })
});

fn shared() -> MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs the helper and returns its exit code, if the process was started
/// as one ([`crate::HELPER_FLAG`] as its first argument); `None` otherwise.
/// Call it first thing in `main`.
///
/// The helper's arguments are the core, MoltenVK, the game and the folder
/// for the core's files.
#[must_use]
pub fn run_helper_if_asked() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != crate::HELPER_FLAG {
        return None;
    }
    let paths: Vec<PathBuf> = args.map(PathBuf::from).collect();
    let [core, moltenvk, game, data] = paths.as_slice() else {
        eprintln!("usage: {} CORE MOLTENVK GAME DATA", crate::HELPER_FLAG);
        return Some(2);
    };
    let code = match serve(core, moltenvk, game, data) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("3DS helper: {error}");
            1
        }
    };
    // The core's static destructors can crash while its threads still
    // run, and a crash would show the player macOS's report for nothing:
    // leave without them. Everything worth keeping is written by now.
    let _ = io::stderr().flush();
    // SAFETY: _exit ends the process at once; nothing after it runs.
    unsafe { libc::_exit(code) }
}

/// Takes the protocol's end of stdout, and sends what anything else in the
/// process prints there (the core, MoltenVK) to stderr, the helper's log,
/// so it cannot break the link.
fn take_stdout() -> io::Result<File> {
    // SAFETY: dup and dup2 take plain file descriptors and change no
    // memory; 1 and 2 are open for the life of the process.
    let link = unsafe { libc::dup(1) };
    if link < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::dup2(2, 1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `link` is a descriptor dup just made, owned by nothing else.
    Ok(unsafe { File::from_raw_fd(link) })
}

/// Loads the game and answers the app's requests until it says to quit
/// or goes.
fn serve(core: &Path, moltenvk: &Path, game: &Path, data: &Path) -> io::Result<()> {
    let mut out = BufWriter::with_capacity(1 << 20, take_stdout()?);
    let mut requests = BufReader::new(io::stdin().lock());
    let mut session = match Session::start(core, moltenvk, game, data) {
        Ok(session) => session,
        Err(why) => {
            link::write_message(&mut out, link::FAILED, &[why.as_bytes()])?;
            return Ok(());
        }
    };
    let renderer = session.renderer.name();
    eprintln!(
        "3DS helper: playing {} with the {renderer} renderer",
        game.display()
    );
    link::write_message(
        &mut out,
        link::HELLO,
        &[&link::VERSION.to_le_bytes(), renderer.as_bytes()],
    )?;
    let mut payload = Vec::new();
    let mut role = Role::of_parent();
    while let Some(kind) = link::read_message(&mut requests, &mut payload)? {
        match kind {
            link::RUN => {
                role.follow();
                let pad = Pad::decode(&payload).unwrap_or_default();
                match session.run(pad) {
                    Ok(_) => session.send_frame(&mut out)?,
                    Err(why) => {
                        link::write_message(&mut out, link::FAILED, &[why.as_bytes()])?;
                        break;
                    }
                }
            }
            link::SAVE => match session.save() {
                Ok(state) => link::write_message(&mut out, link::STATE, &[&state])?,
                Err(why) => link::write_message(&mut out, link::REFUSED, &[why.as_bytes()])?,
            },
            link::LOAD => match session.load(&payload) {
                Ok(()) => link::write_message(&mut out, link::DONE, &[])?,
                Err(why) => link::write_message(&mut out, link::REFUSED, &[why.as_bytes()])?,
            },
            link::RESET => {
                session.reset();
                link::write_message(&mut out, link::DONE, &[])?;
            }
            link::SCALE => {
                let scale = payload
                    .get(..4)
                    .and_then(|bytes| bytes.try_into().ok())
                    .map_or(1, u32::from_le_bytes);
                set_option(
                    "citra_resolution_factor",
                    &scale.clamp(1, MAX_SCALE).to_string(),
                );
                link::write_message(&mut out, link::DONE, &[])?;
            }
            _ => break,
        }
    }
    session.close();
    Ok(())
}

/// Keeps the helper at the app's scheduling role. While the app plays full
/// screen, macOS's Game Mode may demote other processes to its background
/// role, which runs them on efficiency cores with their timers held back,
/// as it did RPG Maker's hidden player (decision 0008). A process may set
/// the role of another of its user's, so the helper takes the app's
/// every few frames.
struct Role {
    app: libc::pid_t,
    frames: u32,
}

impl Role {
    /// `PRIO_DARWIN_ROLE`, from `<sys/resource_private.h>`.
    const PRIO_DARWIN_ROLE: libc::c_int = 6;
    /// How often, in frames, the role is checked: a few times a second.
    const EVERY: u32 = 8;

    fn of_parent() -> Self {
        // SAFETY: getppid has no preconditions.
        let app = unsafe { libc::getppid() };
        Self { app, frames: 0 }
    }

    /// Takes the app's role every [`Role::EVERY`] frames, if it differs.
    fn follow(&mut self) {
        self.frames = self.frames.wrapping_add(1);
        if self.frames % Self::EVERY != 1 {
            return;
        }
        // SAFETY: getpriority and setpriority read and set a scheduling
        // value; a pid that has gone only makes them fail.
        unsafe {
            let wanted = libc::getpriority(Self::PRIO_DARWIN_ROLE, self.app.cast_unsigned());
            // A role is 0 to 7; -1 is an error, as when the app has gone.
            if (0..8).contains(&wanted) && libc::getpriority(Self::PRIO_DARWIN_ROLE, 0) != wanted {
                libc::setpriority(Self::PRIO_DARWIN_ROLE, 0, wanted);
            }
        }
    }
}

/// Changes one of the core's variables, which it reads again before its
/// next frame.
fn set_option(key: &str, value: &str) {
    let Ok(value) = CString::new(value) else {
        return;
    };
    let mut shared = shared();
    if shared.options.get(key) != Some(&value) {
        shared.options.insert(key.to_owned(), value);
        shared.options_changed = true;
    }
}

/// A loaded game.
struct Session {
    api: Api,
    renderer: Renderer,
    /// The Vulkan device, when the GPU draws. Dropped only as the process
    /// ends, after the core.
    gpu: Option<Box<Gpu>>,
    /// Reused for each frame: the picture, then the link's payload.
    picture: Picture,
    frame: Vec<u8>,
    /// Why this game cannot have save states, if it cannot.
    no_states: Option<&'static str>,
}

impl Session {
    /// Opens the core, picks the renderer and loads `game`, saying why
    /// not in words for the player.
    fn start(core: &Path, moltenvk: &Path, game: &Path, data: &Path) -> Result<Self, String> {
        let renderer = match std::env::var("DS_EMULATOR_3DS_RENDERER").as_deref() {
            Ok("software") => Renderer::Software,
            _ => match crate::vulkan::probe(moltenvk) {
                Ok(device) => {
                    eprintln!("3DS helper: Vulkan on {device}");
                    Renderer::Vulkan
                }
                Err(why) => {
                    eprintln!("3DS helper: no Vulkan ({why}); drawing in software");
                    Renderer::Software
                }
            },
        };
        let directory = CString::new(data.as_os_str().as_encoded_bytes())
            .map_err(|_| "the 3DS folder's name has a NUL in it".to_owned())?;
        let path = CString::new(game.as_os_str().as_encoded_bytes())
            .map_err(|_| "the game's file name has a NUL in it".to_owned())?;
        {
            let mut shared = shared();
            shared.options = options(renderer);
            shared.directory = directory;
            shared.renderer = renderer;
        }

        // SAFETY: `core` is Azahar's libretro core, checked against its
        // pinned digest when it was installed.
        let api = unsafe { Api::open(core) }
            .map_err(|error| format!("Could not open the 3DS core: {error}"))?;
        // SAFETY: these are the core's entry points, called in the order
        // libretro gives: callbacks, init, then the game. The callbacks
        // are plain functions that live as long as the process.
        unsafe {
            if (api.api_version)() != lr::RETRO_API_VERSION {
                return Err("The 3DS core speaks another libretro version.".to_owned());
            }
            (api.set_environment)(environment);
            (api.set_video_refresh)(video_refresh);
            (api.set_audio_sample)(audio_sample);
            (api.set_audio_sample_batch)(audio_sample_batch);
            (api.set_input_poll)(input_poll);
            (api.set_input_state)(input_state);
            (api.init)();
        }
        let info = lr::GameInfo {
            path: path.as_ptr(),
            data: std::ptr::null(),
            size: 0,
            meta: std::ptr::null(),
        };
        // SAFETY: `info` and the path it points at outlive the call; the
        // core needs the full path, not the data.
        if !unsafe { (api.load_game)(&info) } {
            return Err(refusal("Azahar could not load this game."));
        }

        let mut session = Self {
            api,
            renderer,
            gpu: None,
            picture: Picture::default(),
            frame: Vec::new(),
            no_states: (renderer == Renderer::Vulkan && !has_title_id(game))
                .then_some(NO_HOMEBREW_STATES),
        };
        let (negotiation, reset) = {
            let shared = shared();
            (shared.negotiation, shared.context_reset)
        };
        if renderer == Renderer::Vulkan {
            let (Some(negotiation), Some(reset)) = (negotiation, reset) else {
                return Err("The 3DS core did not ask for Vulkan.".to_owned());
            };
            // SAFETY: the negotiation interface is the core's, which it
            // keeps for as long as the game is loaded.
            let gpu = unsafe { Gpu::new(moltenvk, &*negotiation.0) }
                .map_err(|why| format!("Could not set up Vulkan for the 3DS core: {why}"))?;
            shared().interface = Some(Handed(gpu.interface()));
            session.gpu = Some(gpu);
            // SAFETY: the interface is in place, as context_reset expects;
            // with Vulkan the core loads the game here.
            unsafe { reset() };
        }
        // Run the first frames now, so that a game the core could not
        // start says so before the app shows it. A game that runs shows a
        // frame each time.
        for _ in 0..STARTUP_FRAMES {
            if session.run(Pad::default())? {
                return Ok(session);
            }
        }
        Err(refusal("Azahar could not start this game."))
    }

    /// Runs one frame with `pad` held, leaving its picture in `picture`
    /// (the last one again if the core repeated it) and its sound in
    /// [`Shared::audio`]. Returns whether the core drew a frame: one that
    /// does not load the game it was given repeats an empty one forever.
    fn run(&mut self, pad: Pad) -> Result<bool, String> {
        {
            let mut shared = shared();
            shared.pad = pad;
            shared.audio.clear();
            shared.gpu_frame = None;
            shared.fresh = false;
            shared.drew = false;
        }
        // SAFETY: the game is loaded; the callbacks take SHARED's lock
        // themselves.
        unsafe { (self.api.run)() };
        let mut shared = shared();
        let drew = shared.drew;
        if let (Some((width, height)), Some(gpu)) = (shared.gpu_frame, &mut self.gpu) {
            drop(shared);
            gpu.read_frame(width, height, &mut self.picture)
                .map_err(|why| format!("Could not read the 3DS core's frame: {why}"))?;
        } else if shared.fresh {
            std::mem::swap(&mut self.picture, &mut shared.picture);
        }
        Ok(drew)
    }

    /// Sends the last frame's screens and sound.
    fn send_frame(&mut self, out: &mut impl Write) -> io::Result<()> {
        let audio = std::mem::take(&mut shared().audio);
        let (top, bottom) = split(&self.picture, &mut self.frame);
        let samples = audio.len() / 2;
        let sound: Vec<u8> = audio
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let result = link::write_message(
            out,
            link::FRAME,
            &[&link::frame_head(top, bottom, samples), &self.frame, &sound],
        );
        // Hand the buffer back for the next frame's sound.
        let mut shared = shared();
        if shared.audio.is_empty() {
            shared.audio = audio;
        }
        result
    }

    /// The machine's state, or why the core gave none.
    fn save(&mut self) -> Result<Vec<u8>, String> {
        if let Some(why) = self.no_states {
            return Err(why.to_owned());
        }
        // SAFETY: the game is loaded; the size call lets the core finish
        // its file work and take the state, which serialize then copies.
        let size = unsafe { (self.api.serialize_size)() };
        if size == 0 {
            return Err("The 3DS core could not save the game just now.".to_owned());
        }
        let mut state = vec![0; size];
        // SAFETY: `state` holds `size` writable bytes.
        if unsafe { (self.api.serialize)(state.as_mut_ptr().cast(), size) } {
            Ok(state)
        } else {
            Err("The 3DS core could not save the game just now.".to_owned())
        }
    }

    /// Loads a state the core made.
    fn load(&mut self, state: &[u8]) -> Result<(), String> {
        if let Some(why) = self.no_states {
            return Err(why.to_owned());
        }
        // SAFETY: `state` holds `state.len()` readable bytes, which the
        // core copies.
        if unsafe { (self.api.unserialize)(state.as_ptr().cast(), state.len()) } {
            Ok(())
        } else {
            Err("The 3DS core could not load that state.".to_owned())
        }
    }

    fn reset(&mut self) {
        // SAFETY: the game is loaded.
        unsafe { (self.api.reset)() };
    }

    /// Lets the core close the game's files.
    fn close(self) {
        // SAFETY: the game is loaded, and nothing calls the core after.
        unsafe { (self.api.unload_game)() };
        let destroy = shared().context_destroy;
        if let Some(destroy) = destroy {
            // SAFETY: the Vulkan device is still up, as the core expects.
            unsafe { destroy() };
        }
        // The core's threads may still hold the device and the code:
        // leave both for the process's end.
        std::mem::forget(self);
    }
}

/// Whether the game at `path` has a title ID, as cartridges, executables
/// and installed titles do and `.3dsx` homebrew doesn't.
fn has_title_id(path: &Path) -> bool {
    let homebrew_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("3dsx") || extension.eq_ignore_ascii_case("z3dsx")
        });
    match crate::header::read(path) {
        Ok(header) => match header.format {
            crate::header::Format::Homebrew => false,
            crate::header::Format::Compressed => !homebrew_extension,
            crate::header::Format::Cartridge | crate::header::Format::Executable => true,
        },
        Err(_) => !homebrew_extension,
    }
}

/// The core's last message if it showed one, else `otherwise`.
fn refusal(otherwise: &str) -> String {
    refusal_from(&shared(), otherwise)
}

fn refusal_from(shared: &Shared, otherwise: &str) -> String {
    shared
        .message
        .clone()
        .unwrap_or_else(|| otherwise.to_owned())
}

/// Cuts `picture`, the core's two screens top over bottom, into the
/// [`link`]'s pixels: the top screen's rows, then the bottom's, written
/// into `bytes`. Returns the two screens' sizes.
fn split(picture: &Picture, bytes: &mut Vec<u8>) -> (ScreenSize, ScreenSize) {
    let scale = (picture.width / PICTURE.0).max(1);
    let (width, height) = (PICTURE.0 * scale, PICTURE.1 * scale);
    let top = (width, height / 2);
    let bottom = ((PICTURE.0 - 2 * BOTTOM_AT.0) * scale, height / 2);
    bytes.clear();
    bytes.reserve((top.0 * top.1 + bottom.0 * bottom.1) as usize * 4);
    let row = |y: u32, from: u32, count: u32, bytes: &mut Vec<u8>| {
        let start = (y * picture.width + from) as usize;
        let pixels = picture.pixels.get(start..start + count as usize);
        match pixels {
            Some(pixels) => extend_pixels(bytes, pixels),
            // A picture of an unexpected size: black rather than garbage.
            None => bytes.extend(std::iter::repeat_n(0, count as usize * 4)),
        }
    };
    let fits = picture.width == width && picture.height == height;
    for y in 0..top.1 {
        if fits {
            row(y, 0, top.0, bytes);
        } else {
            bytes.extend(std::iter::repeat_n(0, top.0 as usize * 4));
        }
    }
    for y in 0..bottom.1 {
        if fits {
            row(
                BOTTOM_AT.1 * scale + y,
                BOTTOM_AT.0 * scale,
                bottom.0,
                bytes,
            );
        } else {
            bytes.extend(std::iter::repeat_n(0, bottom.0 as usize * 4));
        }
    }
    let size = |(width, height): (u32, u32)| {
        ScreenSize::new(
            u16::try_from(width).unwrap_or(u16::MAX),
            u16::try_from(height).unwrap_or(u16::MAX),
        )
    };
    (size(top), size(bottom))
}

/// Appends `pixels` to `bytes`, little-endian, as the link has them.
fn extend_pixels(bytes: &mut Vec<u8>, pixels: &[u32]) {
    #[cfg(target_endian = "little")]
    {
        // SAFETY: a u32 slice is readable as four times as many bytes,
        // which on a little-endian host are each pixel's bytes in order.
        let raw =
            unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), pixels.len() * 4) };
        bytes.extend_from_slice(raw);
    }
    #[cfg(not(target_endian = "little"))]
    bytes.extend(pixels.iter().flat_map(|pixel| pixel.to_le_bytes()));
}

/// Where a touch at `point` on the bottom screen, in native pixels, is on
/// libretro's pointer: -32,767..32,767 across the whole picture.
fn pointer_at(point: (u16, u16)) -> (i16, i16) {
    let axis = |offset: u32, at: u16, across: u32| {
        let fraction = (f64::from(offset) + f64::from(at) + 0.5) / f64::from(across);
        let value = (fraction * 65_534.0 - 32_767.0).round();
        // In range: the fraction lies within 0..1.
        value.clamp(-32_767.0, 32_767.0) as i16
    };
    (
        axis(BOTTOM_AT.0, point.0, PICTURE.0),
        axis(BOTTOM_AT.1, point.1, PICTURE.1),
    )
}

/// libretro's environment callback: the core asking the frontend.
unsafe extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    if data.is_null() {
        return false;
    }
    let mut shared = shared();
    // SAFETY: in each arm, `data` points at what libretro defines for
    // `cmd`, which the core owns for the call.
    unsafe {
        match cmd {
            lr::ENV_GET_SYSTEM_DIRECTORY | lr::ENV_GET_SAVE_DIRECTORY => {
                *data.cast::<*const c_char>() = shared.directory.as_ptr();
                true
            }
            lr::ENV_SET_PIXEL_FORMAT => *data.cast::<c_uint>() == lr::PIXEL_FORMAT_XRGB8888,
            lr::ENV_GET_VARIABLE => {
                let variable = &mut *data.cast::<lr::Variable>();
                let value = (!variable.key.is_null())
                    .then(|| CStr::from_ptr(variable.key).to_str().ok())
                    .flatten()
                    .and_then(|key| shared.options.get(key));
                variable.value = value.map_or(std::ptr::null(), |value| value.as_ptr());
                value.is_some()
            }
            lr::ENV_GET_VARIABLE_UPDATE => {
                *data.cast::<bool>() = std::mem::take(&mut shared.options_changed);
                true
            }
            lr::ENV_SET_VARIABLES
            | lr::ENV_SET_GEOMETRY
            | lr::ENV_SET_SYSTEM_AV_INFO
            | lr::ENV_SET_HW_SHARED_CONTEXT
            | lr::ENV_SET_SERIALIZATION_QUIRKS
            | lr::ENV_GET_INPUT_BITMASKS => true,
            lr::ENV_GET_CAN_DUPE => {
                *data.cast::<bool>() = true;
                true
            }
            lr::ENV_GET_AUDIO_VIDEO_ENABLE => {
                *data.cast::<c_uint>() = 3;
                true
            }
            lr::ENV_SET_MESSAGE => {
                let message = &*data.cast::<lr::Message>();
                note_message(&mut shared, message.msg);
                true
            }
            lr::ENV_SET_MESSAGE_EXT => {
                let message = &*data.cast::<lr::MessageExt>();
                note_message(&mut shared, message.msg);
                true
            }
            lr::ENV_GET_PREFERRED_HW_RENDER => {
                *data.cast::<c_uint>() = lr::HW_CONTEXT_VULKAN;
                shared.renderer == Renderer::Vulkan
            }
            lr::ENV_SET_HW_RENDER => {
                let render = &*data.cast::<lr::HwRenderCallback>();
                if shared.renderer != Renderer::Vulkan
                    || render.context_type != lr::HW_CONTEXT_VULKAN
                {
                    return false;
                }
                shared.context_reset = render.context_reset;
                shared.context_destroy = render.context_destroy;
                true
            }
            lr::ENV_SET_HW_RENDER_CONTEXT_NEGOTIATION_INTERFACE => {
                let negotiation = data.cast::<lr::VulkanNegotiation>().cast_const();
                shared.negotiation = Some(Handed(negotiation));
                true
            }
            lr::ENV_GET_HW_RENDER_INTERFACE => match shared.interface {
                Some(interface) => {
                    *data.cast::<*const c_void>() = interface.0.cast();
                    true
                }
                None => false,
            },
            _ => false,
        }
    }
}

/// Keeps and logs a message the core shows the player.
///
/// # Safety
///
/// `text` is null or a NUL-terminated string.
unsafe fn note_message(shared: &mut Shared, text: *const c_char) {
    if text.is_null() {
        return;
    }
    // SAFETY: the caller vouches for `text`.
    let text = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    // eprintln! panics if the log cannot be written; this runs inside the
    // core's callback, where a panic cannot unwind.
    let _ = writeln!(io::stderr(), "3DS core says: {text}");
    shared.message = Some(text);
}

/// libretro's video callback: a frame in the software renderer's memory,
/// the hardware renderer's image, or null for the last frame again.
unsafe extern "C" fn video_refresh(
    data: *const c_void,
    width: c_uint,
    height: c_uint,
    pitch: usize,
) {
    let mut shared = shared();
    if data == lr::HW_FRAME_BUFFER_VALID {
        shared.gpu_frame = Some((width, height));
        shared.drew = true;
        return;
    }
    // Nothing may panic here, as a panic cannot unwind into the core.
    if data.is_null() || width == 0 || height == 0 || pitch < width as usize * 4 {
        return;
    }
    shared.drew = true;
    shared.fresh = true;
    shared.picture.resize(width, height);
    for (y, row) in shared
        .picture
        .pixels
        .chunks_exact_mut(width as usize)
        .enumerate()
    {
        // SAFETY: the core's frame holds `height` rows of `pitch` bytes,
        // each starting with `width` XRGB8888 pixels.
        let source = unsafe {
            std::slice::from_raw_parts(data.cast::<u8>().add(y * pitch), width as usize * 4)
        };
        for (pixel, word) in row.iter_mut().zip(source.chunks_exact(4)) {
            *pixel = 0xFF00_0000 | u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
    }
}

unsafe extern "C" fn audio_sample(left: i16, right: i16) {
    shared().audio.extend([left, right]);
}

unsafe extern "C" fn audio_sample_batch(data: *const i16, frames: usize) -> usize {
    if !data.is_null() {
        // SAFETY: the core's batch holds `frames` stereo samples.
        let samples = unsafe { std::slice::from_raw_parts(data, frames * 2) };
        shared().audio.extend_from_slice(samples);
    }
    frames
}

unsafe extern "C" fn input_poll() {}

/// libretro's input callback: what the player holds, as the core asks.
unsafe extern "C" fn input_state(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16 {
    if port != 0 {
        return 0;
    }
    let mut shared = shared();
    let pad = shared.pad;
    match device {
        lr::DEVICE_JOYPAD if id == lr::JOYPAD_MASK => pad.buttons.cast_signed(),
        lr::DEVICE_JOYPAD if id < 16 => i16::from(pad.buttons >> id & 1 != 0),
        lr::DEVICE_ANALOG if index == lr::ANALOG_LEFT => match id {
            lr::ANALOG_X => pad.circle.0,
            lr::ANALOG_Y => pad.circle.1,
            _ => 0,
        },
        lr::DEVICE_POINTER => {
            if let Some(point) = pad.touch {
                shared.pointer = pointer_at(point);
            }
            match id {
                lr::POINTER_X => shared.pointer.0,
                lr::POINTER_Y => shared.pointer.1,
                lr::POINTER_PRESSED | lr::POINTER_COUNT => i16::from(pad.touch.is_some()),
                _ => 0,
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(scale: u32) -> Picture {
        let mut picture = Picture::default();
        picture.resize(PICTURE.0 * scale, PICTURE.1 * scale);
        for (index, pixel) in picture.pixels.iter_mut().enumerate() {
            let (x, y) = (index as u32 % picture.width, index as u32 / picture.width);
            *pixel = 0xFF00_0000 | y << 12 | x;
        }
        picture
    }

    fn pixel(bytes: &[u8], index: usize) -> u32 {
        u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
    }

    #[test]
    fn the_picture_splits_into_the_two_screens() {
        let mut bytes = Vec::new();
        let (top, bottom) = split(&picture(1), &mut bytes);
        assert_eq!((top, bottom), (crate::SCREENS[0], crate::SCREENS[1]));
        assert_eq!(bytes.len(), (400 * 240 + 320 * 240) * 4);
        assert_eq!(pixel(&bytes, 0), 0xFF00_0000);
        assert_eq!(pixel(&bytes, 400 * 240 - 1), 0xFF00_0000 | 239 << 12 | 399);
        let bottom_start = 400 * 240;
        assert_eq!(pixel(&bytes, bottom_start), 0xFF00_0000 | 240 << 12 | 40);
        assert_eq!(
            pixel(&bytes, bottom_start + 320 * 240 - 1),
            0xFF00_0000 | 479 << 12 | 359
        );
    }

    #[test]
    fn a_larger_picture_splits_at_its_scale() {
        let mut bytes = Vec::new();
        let (top, bottom) = split(&picture(3), &mut bytes);
        assert_eq!(top, ScreenSize::new(1200, 720));
        assert_eq!(bottom, ScreenSize::new(960, 720));
        assert_eq!(pixel(&bytes, 1200 * 720), 0xFF00_0000 | 720 << 12 | 120);
    }

    #[test]
    fn an_odd_picture_comes_out_black_at_the_right_sizes() {
        let mut odd = Picture::default();
        odd.resize(256, 384);
        let mut bytes = Vec::new();
        let (top, bottom) = split(&odd, &mut bytes);
        assert_eq!((top, bottom), (crate::SCREENS[0], crate::SCREENS[1]));
        assert!(bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn touches_land_on_the_bottom_screen_of_the_picture() {
        // The core maps the pointer back with (p + 32767) / 65534 × size.
        let back = |(x, y): (i16, i16)| {
            let to = |value: i16, across: f64| {
                ((f64::from(value) + 32_767.0) / 65_534.0 * across) as u32
            };
            (to(x, 400.0), to(y, 480.0))
        };
        assert_eq!(back(pointer_at((0, 0))), (40, 240));
        assert_eq!(back(pointer_at((319, 239))), (359, 479));
        assert_eq!(back(pointer_at((160, 120))), (200, 360));
    }
}
