//! Plays Nintendo 3DS games on Azahar's libretro core.
//!
//! Azahar (GPL-3.0) is downloaded at first use, never shipped with the app
//! ([`install`]), and runs in a helper process: the app's own executable,
//! started again with [`HELPER_FLAG`], which calls
//! [`run_helper_if_asked`] before anything else. The helper loads the
//! core, gives it a Vulkan device through MoltenVK, and runs one frame
//! each time [`ThreeDs`] asks, sending back both screens and the frame's
//! sound over its stdin and stdout.
//!
//! Like the RPG Maker core, this one is not deterministic: the game's
//! clock and the core's threads are its own. States are the whole
//! machine, so they save and load anywhere, but take a few hundred
//! milliseconds, so there is no rewind.
//!
//! The core cannot install a title, so an installable title (`.cia`)
//! plays as the executable it holds, which [`cia::extract`] writes out
//! first.

pub mod cia;
pub mod header;
pub mod install;

#[allow(unsafe_code)]
mod host;
#[allow(unsafe_code)]
mod libretro;
mod link;
#[allow(unsafe_code)]
mod vulkan;

use std::path::{Path, PathBuf};

use emu_core::{
    Button, Core, CoreInfo, Frame, FrameRate, Input, ScreenSize, StateError, StereoSample, System,
};

pub use host::run_helper_if_asked;

/// The argument that starts the app's executable as the helper.
pub const HELPER_FLAG: &str = "--3ds-helper";

/// The file extensions of 3DS games: cartridge images (`.3ds`, `.cci`),
/// executables (`.cxi`), homebrew (`.3dsx`) and Azahar's compressed forms
/// of each, which the core opens; and installable titles (`.cia`), which
/// the core cannot install, so they play as the executable they hold
/// ([`cia::extract`]).
pub const EXTENSIONS: [&str; 8] = ["3ds", "cci", "cxi", "3dsx", "zcci", "zcxi", "z3dsx", "cia"];

/// Whether `path` names a 3DS game, by its extension.
#[must_use]
pub fn is_game(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Where the core, MoltenVK and the emulated console's own files (its
/// NAND, SD card and the games' saves) are kept:
/// `~/Library/Application Support/ds-emulator/3DS`.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("ds-emulator").join("3DS"))
}

/// Where the core keeps one game's own saves on the emulated SD card: the
/// save data Azahar gives the title,
/// `Azahar/sdmc/Nintendo 3DS/<ID0>/<ID1>/title/<high>/<low>/data/` (beside
/// `content/`, an installed title's own files, which are not saves), and
/// the extra data (extdata) some games keep as well, in
/// `…/<ID0>/<ID1>/extdata/<HIGH>/<LOW>/`, named by the extdata ID in the
/// game's extended header. Azahar names a title's folders in lowercase
/// hex and an extdata's in capitals. The app fingerprints these files
/// beside a checkpoint, so that a checkpoint is never picked up over a
/// save the game made since (see `docs/decisions/0011-3ds-games.md`).
/// Homebrew without a title ID keeps its files where it likes, so it has
/// none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveData {
    /// `Azahar/sdmc/Nintendo 3DS` in the data folder.
    sd: PathBuf,
    /// The game's folders in each `<ID0>/<ID1>` folder there: its save
    /// data's, then its extdata's if it has any.
    folders: Vec<PathBuf>,
}

impl SaveData {
    /// The save data of the game at `game`, whose console's files are in
    /// `data` ([`data_dir`]).
    #[must_use]
    pub fn of(game: &Path, data: &Path) -> Self {
        let header = header::read(game).ok();
        let save_data = header
            .as_ref()
            .and_then(|header| header.title_id)
            .map(|id| {
                let (high, low) = halves(id);
                Path::new("title")
                    .join(format!("{high:08x}"))
                    .join(format!("{low:08x}"))
                    .join("data")
            });
        let extdata = header.and_then(|header| header.extdata_id).map(|id| {
            let (high, low) = halves(id);
            Path::new("extdata")
                .join(format!("{high:08X}"))
                .join(format!("{low:08X}"))
        });
        Self {
            sd: data.join("Azahar").join("sdmc").join("Nintendo 3DS"),
            folders: save_data.into_iter().chain(extdata).collect(),
        }
    }

    /// Every file in the game's save data and extdata now, sorted.
    #[must_use]
    pub fn files(&self) -> Vec<PathBuf> {
        if self.folders.is_empty() {
            return Vec::new();
        }
        // The two IDs are the console's and its SD card's; Azahar makes
        // one of each, but any will do.
        let mut files: Vec<PathBuf> = subfolders(&self.sd)
            .flat_map(|id0| subfolders(&id0).collect::<Vec<_>>())
            .flat_map(|ids| self.folders.iter().map(move |folder| ids.join(folder)))
            .flat_map(|folder| files_under(&folder))
            .collect();
        files.sort();
        files
    }
}

/// The high and low halves of a 64-bit ID, as Azahar names its folders.
fn halves(id: u64) -> (u32, u32) {
    ((id >> 32) as u32, id as u32)
}

/// The folders directly in `folder`.
fn subfolders(folder: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.is_dir())
}

/// Every file under `folder`, at any depth.
fn files_under(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![folder.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() => files.push(path),
                _ => {}
            }
        }
    }
    files
}

/// The top screen, then the bottom (touch) screen, at native resolution.
pub const SCREENS: [ScreenSize; 2] = [ScreenSize::new(400, 240), ScreenSize::new(320, 240)];

/// The 3DS's frame rate: the ARM11's 268,111,856 Hz clock over 4,481,136
/// cycles a frame, about 59.83 fps.
pub const FRAME_RATE: FrameRate = FrameRate::new(268_111_856, 4_481_136);

/// The rate of the core's sound: the ARM11's clock over 8,192.
pub const SAMPLE_RATE: u32 = 32_728;

/// Static facts about the 3DS core.
pub const INFO: CoreInfo = CoreInfo {
    system: System::N3ds,
    screens: &SCREENS,
    touch_screen: Some(1),
    players: 1,
    frame_rate: FRAME_RATE,
    audio_sample_rate: SAMPLE_RATE,
    state_version: 1,
};

/// Why a 3DS game could not start.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The core or MoltenVK could not be installed.
    #[error(transparent)]
    Install(#[from] install::InstallError),
    /// The helper could not be started or stopped answering.
    #[error("the 3DS helper could not start: {0}")]
    Start(String),
    /// The core refused the game, saying why.
    #[error("{0}")]
    Game(String),
}

/// A 3DS game running on Azahar's core in a helper process.
pub struct ThreeDs {
    link: std::cell::RefCell<link::Client>,
    screens: [Frame; 2],
    ended: Option<String>,
}

impl ThreeDs {
    /// Starts `game` in a helper process. `helper` is the executable to
    /// start with [`HELPER_FLAG`] (the app's own, which calls
    /// [`run_helper_if_asked`] first thing), and `data` is where the core
    /// and the console's files are kept ([`data_dir`]); the core is
    /// downloaded there first if it is missing.
    ///
    /// Blocks until the game's first frame can run: call it off the UI
    /// thread.
    ///
    /// # Errors
    ///
    /// Fails if the core cannot be installed, the helper cannot start, or
    /// the core refuses the game, as it does an encrypted one.
    pub fn open(game: &Path, data: &Path, helper: &Path) -> Result<Self, OpenError> {
        let parts = install::ensure(data)?;
        Self::open_with(game, data, helper, &parts)
    }

    /// Starts `game` as [`ThreeDs::open`] does, on the core and MoltenVK
    /// in `parts` rather than the installed ones.
    ///
    /// # Errors
    ///
    /// Fails if the helper cannot start or the core refuses the game.
    pub fn open_with(
        game: &Path,
        data: &Path,
        helper: &Path,
        parts: &install::Parts,
    ) -> Result<Self, OpenError> {
        let link = link::Client::start(helper, game, data, parts)?;
        Ok(Self {
            link: std::cell::RefCell::new(link),
            screens: SCREENS.map(Frame::new),
            ended: None,
        })
    }

    /// Notes that the helper has gone, with why, and reports it from
    /// [`Core::ended`] from then on.
    fn end(&mut self, why: String) {
        if self.ended.is_none() {
            self.ended = Some(why);
        }
    }
}

/// The libretro joypad bits for `input`'s buttons. The D-pad also tilts
/// the circle pad all the way (see [`circle_pad`]).
fn joypad_bits(input: &Input) -> u16 {
    use libretro::{
        JOYPAD_A, JOYPAD_B, JOYPAD_DOWN, JOYPAD_L, JOYPAD_L2, JOYPAD_LEFT, JOYPAD_R, JOYPAD_R2,
        JOYPAD_RIGHT, JOYPAD_SELECT, JOYPAD_START, JOYPAD_UP, JOYPAD_X, JOYPAD_Y,
    };
    let map = [
        (Button::A, JOYPAD_A),
        (Button::B, JOYPAD_B),
        (Button::X, JOYPAD_X),
        (Button::Y, JOYPAD_Y),
        (Button::L, JOYPAD_L),
        (Button::R, JOYPAD_R),
        (Button::Zl, JOYPAD_L2),
        (Button::Zr, JOYPAD_R2),
        (Button::Start, JOYPAD_START),
        (Button::Select, JOYPAD_SELECT),
        (Button::Up, JOYPAD_UP),
        (Button::Down, JOYPAD_DOWN),
        (Button::Left, JOYPAD_LEFT),
        (Button::Right, JOYPAD_RIGHT),
    ];
    map.iter()
        .filter(|(button, _)| input.buttons.contains(*button))
        .fold(0, |bits, (_, id)| bits | 1 << id)
}

/// The circle pad's tilt for `input`, from -32,767 to 32,767 on each axis
/// with up negative, as libretro has it. The 3DS moves with the circle pad
/// where the DS moved with the D-pad, so the D-pad's bindings (WASD, the
/// arrow keys and a gamepad's left stick) tilt it all the way, as well as
/// pressing the D-pad itself; a diagonal tilts it as far along each axis
/// as a real stick would reach.
fn circle_pad(input: &Input) -> (i16, i16) {
    const FULL: i32 = i16::MAX as i32;
    const DIAGONAL: i32 = 23_170; // FULL / √2
    let axis = |less: Button, more: Button| {
        i32::from(input.buttons.contains(more)) - i32::from(input.buttons.contains(less))
    };
    let (x, y) = (
        axis(Button::Left, Button::Right),
        axis(Button::Up, Button::Down),
    );
    let reach = if x != 0 && y != 0 { DIAGONAL } else { FULL };
    let tilt = |direction: i32| i16::try_from(direction * reach).unwrap_or_default();
    (tilt(x), tilt(y))
}

impl Core for ThreeDs {
    fn info(&self) -> CoreInfo {
        INFO
    }

    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>) {
        if self.ended().is_some() {
            return;
        }
        let input = inputs.first().copied().unwrap_or_default();
        let pad = link::Pad {
            buttons: joypad_bits(&input),
            circle: circle_pad(&input),
            touch: input.touch.map(|point| (point.x, point.y)),
        };
        let result = self.link.get_mut().run(pad, &mut self.screens, audio);
        if let Err(why) = result {
            self.end(why);
        }
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn set_render_scale(&mut self, scale: u32) {
        if self.ended.is_none()
            && let Err(why) = self.link.get_mut().set_scale(scale)
        {
            self.end(why);
        }
    }

    fn reset(&mut self) {
        if self.ended.is_none()
            && let Err(why) = self.link.get_mut().reset()
        {
            self.end(why);
        }
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        // A state the core cannot make is reported by try_save_state; the
        // app only calls this to keep one to go back to, where none is
        // fine.
        let _ = self.try_save_state(out);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        if self.ended().is_some() {
            return Err(StateError::Malformed("the game has stopped"));
        }
        self.link.get_mut().load_state(state).map_err(|why| {
            // StateError carries only fixed words: keep the one reason the
            // player can act on.
            StateError::Malformed(if why == host::NO_HOMEBREW_STATES {
                host::NO_HOMEBREW_STATES
            } else {
                "the 3DS core could not load it"
            })
        })
    }

    fn try_save_state(&self, out: &mut Vec<u8>) -> Result<(), String> {
        if let Some(why) = self.ended() {
            return Err(why);
        }
        self.link.borrow_mut().save_state(out)
    }

    fn saves_every_frame(&self) -> bool {
        false
    }

    fn checkpoints_while_playing(&self) -> bool {
        // A state takes a few hundred milliseconds, which would hitch play
        // every minute, and 3DS games save themselves.
        false
    }

    fn ended(&self) -> Option<String> {
        self.ended
            .clone()
            .or_else(|| self.link.borrow().dead().map(str::to_owned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emu_core::{Buttons, TouchPoint};

    fn holding(buttons: &[Button]) -> Input {
        Input {
            buttons: buttons.iter().copied().collect::<Buttons>(),
            ..Input::default()
        }
    }

    #[test]
    fn extensions_are_recognised_in_any_case() {
        assert!(is_game(Path::new("/games/Game.3DS")));
        assert!(is_game(Path::new("game.cci")));
        assert!(is_game(Path::new("homebrew.3dsx")));
        assert!(is_game(Path::new("Title.CIA")));
        assert!(!is_game(Path::new("game.nds")));
        assert!(!is_game(Path::new("Player.app")));
    }

    #[test]
    fn a_games_save_data_is_its_titles_folder_on_the_sd_card() {
        let root = std::env::temp_dir().join(format!("emu-3ds-save-data-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ids = root.join("Azahar/sdmc/Nintendo 3DS/0000/1111");
        let mine = ids.join("title/00040000/0011c400/data/00000001");
        let other = ids.join("title/00040000/00055d00/data/00000001");
        let content = ids.join("title/00040000/0011c400/content");
        for folder in [&mine, &other, &content] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("main"), b"save").unwrap();
        }
        let saves = SaveData {
            sd: root.join("Azahar/sdmc/Nintendo 3DS"),
            folders: vec![PathBuf::from("title/00040000/0011c400/data")],
        };
        assert_eq!(saves.files(), [mine.join("main")]);
        assert!(SaveData::default().files().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_games_extdata_is_watched_beside_its_save_data() {
        let root = std::env::temp_dir().join(format!("emu-3ds-extdata-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // header::tests::ncch's title ID and extdata ID.
        let icon = header::tests::smdh(&[(1, "Test Game")]);
        let game = root.join("game.cxi");
        std::fs::write(&game, header::tests::ncch(true, &icon)).unwrap();
        let data = root.join("3DS");
        let ids = data.join("Azahar/sdmc/Nintendo 3DS/0000/1111");
        let save = ids.join("title/00040000/00123400/data/00000001/main");
        let extdata = ids.join("extdata/00000000/000123AB/user/progress");
        let other = ids.join("extdata/00000000/00055D00/user/progress");
        for file in [&save, &extdata, &other] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"save").unwrap();
        }
        assert_eq!(SaveData::of(&game, &data).files(), [extdata, save]);

        let encrypted = root.join("encrypted.cxi");
        std::fs::write(&encrypted, header::tests::ncch(false, &icon)).unwrap();
        assert_eq!(
            SaveData::of(&encrypted, &data).files().len(),
            1,
            "its extended header can't be read"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_frame_rate_is_the_3ds_s() {
        assert!((FRAME_RATE.hz() - 59.83).abs() < 0.01);
        assert_eq!(SAMPLE_RATE, 268_111_856 / 8_192);
    }

    #[test]
    fn buttons_map_to_libretro_bits() {
        assert_eq!(joypad_bits(&holding(&[Button::A])), 1 << 8);
        assert_eq!(joypad_bits(&holding(&[Button::B, Button::Zr])), 1 | 1 << 13);
        assert_eq!(joypad_bits(&Input::default()), 0);
    }

    #[test]
    fn the_d_pad_tilts_the_circle_pad() {
        assert_eq!(circle_pad(&Input::default()), (0, 0));
        assert_eq!(circle_pad(&holding(&[Button::Up])), (0, -i16::MAX));
        assert_eq!(circle_pad(&holding(&[Button::Right])), (i16::MAX, 0));
        assert_eq!(
            circle_pad(&holding(&[Button::Left, Button::Down])),
            (-23_170, 23_170)
        );
        assert_eq!(circle_pad(&holding(&[Button::Left, Button::Right])), (0, 0));
        let touching = Input {
            touch: Some(TouchPoint { x: 1, y: 2 }),
            ..Input::default()
        };
        assert_eq!(circle_pad(&touching), (0, 0));
    }
}
