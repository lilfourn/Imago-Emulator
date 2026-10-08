//! Cheat codes.
//!
//! A cheat list is plain text, kept next to the ROM as `game.cht`:
//!
//! ```text
//! # Comments start with "#".
//! [Infinite money] ards
//! # Hold L+R.
//! 94000130 FCFF0000
//! 62111938 00000000
//! B2111938 00000000
//! 20001DE4 000000FF
//! D2000000 00000000
//! [Walk through walls] off
//! 02000F04:01
//! ```
//!
//! Each cheat is a `[name]` line followed by its codes. After the name may
//! come `on` or `off` and a device tag naming the code format, in either
//! order. Without a tag the format is detected from the codes. Spaces and
//! line breaks inside codes don't matter: the hex digits are cut into
//! codes of the device's length. Game Genie codes, 6 or 9 digits long, are
//! the exception: spaces, line breaks or `+` separate them, and a dash in
//! a Game Boy cheat marks it as one. [`parse`] reads the text into
//! [`Cheat`]s and [`apply`] runs the enabled ones against a [`Memory`],
//! once per frame, the way cheat devices did. Codes that patch the ROM
//! instead don't run: [`rom_patches`] collects them for the core, which
//! applies them to every ROM read.
//!
//! | Tag | Device | System | Code |
//! |---|---|---|---|
//! | `raw` | memory patch | all | `ADDRESS:VALUE`, 1, 2 or 4 bytes by the value's digits |
//! | `gs` | GameShark | Game Boy | `TTVVAAAA`: `01`, and `8x`/`9x` bank writes |
//! | `gg` | Game Genie | Game Boy | `ABC-DEF` or `ABC-DEF-GHI`, ROM patches |
//! | `cb` | CodeBreaker, Xploder | GBA | `XXXXXXXX YYYY`, encrypted after a `9` code |
//! | `gsa` | GameShark Advance, Action Replay v1/v2 | GBA | `XXXXXXXX YYYYYYYY`, encrypted |
//! | `par3` | Pro Action Replay v3, Action Replay MAX | GBA | `XXXXXXXX YYYYYYYY`, encrypted |
//! | `gsa-raw`, `par3-raw` | the same, unencrypted | GBA | `XXXXXXXX YYYYYYYY` |
//! | `ards` | Action Replay DS | DS | `XXXXXXXX YYYYYYYY` |
//!
//! Raw patches may sit among the codes of any device but the Action
//! Replay DS. Encryption state carries from one cheat to the next, as on
//! the devices: a CodeBreaker `9` code or a `DEADFACE` seed change applies
//! to every code after it in the file. Game IDs, hooks and master codes
//! are ignored, since the emulator applies cheats once per frame itself.
//! What each format leaves out is in its module: the devices' own
//! buttons, and Action Replay DS types `C4`–`C6`.

mod ards;
mod cht;
mod codebreaker;
mod codes;
mod decode;
mod detect;
mod device;
mod game_genie;
mod gameshark;
mod gsa;
mod ops;
mod par3;
mod raw;
mod seed_tables;
mod tea;

use std::fmt;

use emu_core::{RomPatch, System};

use crate::codes::Item;
use crate::decode::decode;
use crate::detect::Guess;
use crate::ops::Op;
use crate::tea::Cipher;

pub use device::Device;

/// Memory a cheat reads and writes: one player's CPU address space.
pub trait Memory {
    /// The byte at `address`, or `None` if it cannot be read without side
    /// effects.
    fn peek(&self, address: u32) -> Option<u8>;

    /// Writes the byte at `address`. Returns whether it was plain memory.
    fn poke(&mut self, address: u32, value: u8) -> bool;

    /// Writes the byte at `address` in RAM bank `bank` of the switchable
    /// region holding it, whichever bank is mapped. Returns whether it was
    /// plain memory. Only the Game Boy has such banks; by default nothing
    /// is written.
    fn poke_bank(&mut self, bank: u8, address: u32, value: u8) -> bool {
        let _ = (bank, address, value);
        false
    }

    /// Writes the 16-bit I/O register at `address` (halfword-aligned) as a
    /// CPU store would, side effects and all. Returns whether it was
    /// written: registers whose write would halt the CPU refuse. Only the
    /// GBA's cheats write registers; by default nothing is written.
    fn poke_io_16(&mut self, address: u32, value: u16) -> bool {
        let _ = (address, value);
        false
    }
}

/// A named group of codes, switched on and off together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cheat {
    /// The name from the `[name]` line.
    pub name: String,
    /// Whether [`apply`] runs it.
    pub enabled: bool,
    /// The code format, from the tag or detected.
    pub device: Device,
    program: Program,
}

/// What a cheat runs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Program {
    /// One operation per code.
    Ops(Vec<Op>),
    /// An Action Replay DS program.
    ActionReplayDs(ards::Program),
}

/// A line of a cheat list that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The line number, from 1.
    pub line: usize,
    /// The device the line was read as, if one was chosen.
    pub device: Option<Device>,
    /// What is wrong with it.
    pub message: String,
}

impl ParseError {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            device: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.device {
            Some(device) => write!(f, "line {} ({device}): {}", self.line, self.message),
            None => write!(f, "line {}: {}", self.line, self.message),
        }
    }
}

impl std::error::Error for ParseError {}

/// Encryption state, carried from cheat to cheat through a list.
#[derive(Debug, Clone)]
struct Keys {
    game_shark_advance: Cipher,
    pro_action_replay: Cipher,
    code_breaker: Option<codebreaker::Key>,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            game_shark_advance: Cipher::GAMESHARK_ADVANCE,
            pro_action_replay: Cipher::PRO_ACTION_REPLAY,
            code_breaker: None,
        }
    }
}

/// Reads a cheat list for `system`. Codes before the first `[name]` line
/// form a cheat named "Unnamed".
///
/// # Errors
///
/// Returns the first line that is neither a comment, a name nor a code
/// that a device for `system` can run, naming the device it was read as.
pub fn parse(system: System, text: &str) -> Result<Vec<Cheat>, ParseError> {
    let mut keys = Keys::default();
    cht::sections(text)?
        .into_iter()
        .map(|section| {
            let (device, program) = compile(system, &section, &mut keys)?;
            Ok(Cheat {
                name: section.name,
                enabled: section.enabled,
                device,
                program,
            })
        })
        .collect()
}

/// Runs every enabled cheat against `memory`, each from a fresh start.
pub fn apply(cheats: &[Cheat], memory: &mut impl Memory) {
    for cheat in cheats.iter().filter(|cheat| cheat.enabled) {
        match &cheat.program {
            Program::Ops(list) => ops::run(list, memory),
            Program::ActionReplayDs(program) => program.run(memory),
        }
    }
}

/// The ROM patches of every enabled cheat, in list order, for
/// `emu_core::Core::set_rom_patches`. Hand the core a fresh set whenever
/// the list changes: switching a cheat off or removing it drops its
/// patches.
#[must_use]
pub fn rom_patches(cheats: &[Cheat]) -> Vec<RomPatch> {
    cheats
        .iter()
        .filter(|cheat| cheat.enabled)
        .filter_map(|cheat| match &cheat.program {
            Program::Ops(list) => Some(list),
            Program::ActionReplayDs(_) => None,
        })
        .flatten()
        .filter_map(|op| match op {
            Op::PatchRom(patches) => Some(patches),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

/// Reads one cheat's lines as its tag's device, or the detected one.
fn compile(
    system: System,
    section: &cht::Section<'_>,
    keys: &mut Keys,
) -> Result<(Device, Program), ParseError> {
    let lines = &section.lines;
    let first = lines.first().map_or(section.line, |&(line, _)| line);
    let device = match section.device {
        Some(device) => {
            if device.system().is_some_and(|only| only != system) {
                let only = device.system().map_or("", System::name);
                return Err(ParseError {
                    device: Some(device),
                    ..ParseError::new(section.line, format!("this device is for the {only}"))
                });
            }
            device
        }
        None => match detect::guess(system, lines).map_err(|(l, m)| ParseError::new(l, m))? {
            Guess::Device(device) => device,
            Guess::Gba16 => {
                let items = items(lines, Device::GameSharkAdvance)?;
                detect_gba16(&items, keys).map_err(|message| ParseError::new(first, message))?
            }
        },
    };
    let in_device = |(line, message): (usize, String)| ParseError {
        line,
        device: Some(device),
        message,
    };
    let program = match device {
        Device::Raw => {
            let ops = lines
                .iter()
                .map(|&(line, text)| raw::patch(text).map_err(|e| in_device((line, e))))
                .collect::<Result<_, _>>()?;
            Program::Ops(ops)
        }
        Device::GameGenie => Program::Ops(game_genie::read(lines).map_err(in_device)?),
        Device::ActionReplayDs => {
            Program::ActionReplayDs(ards::Program::new(&items(lines, device)?).map_err(in_device)?)
        }
        Device::GameShark => Program::Ops(
            decode(&items(lines, device)?, &mut gameshark::GameShark).map_err(in_device)?,
        ),
        Device::CodeBreaker => {
            let mut decoder = codebreaker::CodeBreaker::new(&mut keys.code_breaker);
            Program::Ops(decode(&items(lines, device)?, &mut decoder).map_err(in_device)?)
        }
        Device::GameSharkAdvance
        | Device::GameSharkAdvanceRaw
        | Device::ProActionReplay
        | Device::ProActionReplayRaw => {
            Program::Ops(compile_gba16(device, &items(lines, device)?, keys).map_err(in_device)?)
        }
    };
    Ok((device, program))
}

/// `lines` cut into `device`'s codes.
fn items(lines: &[(usize, &str)], device: Device) -> Result<Vec<(usize, Item)>, ParseError> {
    let digits = device.digits().unwrap_or(16);
    codes::items(lines, digits).map_err(|(line, message)| ParseError {
        line,
        device: Some(device),
        message,
    })
}

/// `items` decrypted as `device`'s, updating its seeds in `keys`.
fn decrypted(device: Device, items: &[(usize, Item)], keys: &mut Keys) -> Vec<(usize, Item)> {
    let mut items = items.to_vec();
    match device {
        Device::GameSharkAdvance => keys.game_shark_advance.decrypt_items(&mut items),
        Device::ProActionReplay => keys.pro_action_replay.decrypt_items(&mut items),
        _ => {}
    }
    items
}

/// Reads 16-digit GBA codes as `device`'s.
fn compile_gba16(
    device: Device,
    items: &[(usize, Item)],
    keys: &mut Keys,
) -> Result<Vec<Op>, (usize, String)> {
    let plain = decrypted(device, items, keys);
    match device {
        Device::GameSharkAdvance | Device::GameSharkAdvanceRaw => {
            decode(&plain, &mut gsa::GameSharkAdvance::default())
        }
        _ => decode(&plain, &mut par3::ProActionReplay::default()),
    }
}

/// Which 16-digit GBA device reads `items` most plausibly.
fn detect_gba16(items: &[(usize, Item)], keys: &Keys) -> Result<Device, String> {
    let scores = detect::GBA16.map(|device| {
        let plain = decrypted(device, items, &mut keys.clone());
        let codes: Vec<u64> = plain
            .iter()
            .filter_map(|(_, item)| match item {
                Item::Code(code) => Some(*code),
                Item::Patch(_) => None,
            })
            .collect();
        let score = match device {
            Device::GameSharkAdvance | Device::GameSharkAdvanceRaw => gsa::plausibility(&codes),
            _ => par3::plausibility(&codes),
        };
        (device, score)
    });
    detect::choose_gba16(&scores, |first, second| {
        let read = |device| compile_gba16(device, items, &mut keys.clone());
        matches!((read(first), read(second)), (Ok(a), Ok(b)) if a == b)
    })
}

#[cfg(test)]
mod test_memory;
#[cfg(test)]
mod tests;
