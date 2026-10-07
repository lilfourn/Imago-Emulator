//! Picking the device for a cheat without a tag.
//!
//! Most systems have one code shape per device, so the digits per code
//! decide. GBA codes of 16 digits are the exception: GameShark Advance and
//! Pro Action Replay v3 codes look alike, encrypted or not. Each reading
//! is scored for plausibility (known code types, addresses in RAM, values
//! that fit, and the ID and seed codes as near certainties) and the best
//! one wins. The idea is mGBA's (`GBACheatAddAutodetect`, MPL-2.0); the
//! scores here are this crate's own.

use emu_core::System;

use crate::codes::hex_digits;
use crate::{Device, raw};

/// An ID or seed code: as good as a tag.
pub(crate) const CERTAIN: i32 = 100;
/// A code of a known type.
pub(crate) const CODE: i32 = 4;
/// A value with bits beyond its width, or a field that should be zero.
pub(crate) const MISFIT: i32 = -4;
/// A code of no known type.
pub(crate) const UNKNOWN: i32 = -16;

/// How plausible a GBA cheat's target address is.
pub(crate) fn region(address: u32) -> i32 {
    match address {
        // EWRAM and IWRAM.
        0x0200_0000..0x0204_0000 | 0x0300_0000..0x0300_8000 => 4,
        // I/O registers.
        0x0400_0000..0x0400_0400 => 2,
        _ => -8,
    }
}

/// [`MISFIT`] if `value` has bits outside `mask`.
pub(crate) fn fits(value: u32, mask: u32) -> i32 {
    if value & !mask == 0 { 0 } else { MISFIT }
}

/// What a cheat's code lines look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Guess {
    /// One device fits.
    Device(Device),
    /// GBA codes of 16 digits: see [`choose_gba16`].
    Gba16,
}

/// Guesses the device for an untagged cheat on `system` from the shape of
/// its code lines. Raw patches fit any device; a cheat of only patches is
/// [`Device::Raw`].
///
/// # Errors
///
/// Returns the line and reason for a line that is not a code, or GBA codes
/// whose length fits neither CodeBreaker nor the 16-digit formats, or
/// both.
pub(crate) fn guess(system: System, lines: &[(usize, &str)]) -> Result<Guess, (usize, String)> {
    let codes: Vec<(usize, &str)> = lines
        .iter()
        .copied()
        .filter(|(_, text)| !raw::is_patch(text))
        .collect();
    let Some(&(first, _)) = codes.first() else {
        return Ok(Guess::Device(Device::Raw));
    };
    if system == System::Gb && codes.iter().any(|(_, text)| text.contains('-')) {
        return Ok(Guess::Device(Device::GameGenie));
    }
    let mut lengths = Vec::with_capacity(codes.len());
    for &(line, text) in &codes {
        let digits = hex_digits(text).ok_or_else(|| (line, format!("not a code: {text:?}")))?;
        lengths.push(digits.len());
    }
    match system {
        System::Gb => Ok(Guess::Device(Device::GameShark)),
        System::Nds => Ok(Guess::Device(Device::ActionReplayDs)),
        System::Gba => gba_shape(&lengths).map_err(|message| (first, message)),
    }
}

/// CodeBreaker (12 digits) or the 16-digit formats, from each line's
/// digit count, or else from the total.
fn gba_shape(lengths: &[usize]) -> Result<Guess, String> {
    let all = |digits: usize| lengths.iter().all(|length| length % digits == 0);
    let total: usize = lengths.iter().sum();
    let (twelve, sixteen) = match (all(12), all(16)) {
        (true, true) | (false, false) => (total.is_multiple_of(12), total.is_multiple_of(16)),
        each => each,
    };
    match (twelve, sixteen) {
        (true, false) => Ok(Guess::Device(Device::CodeBreaker)),
        (false, true) => Ok(Guess::Gba16),
        (true, true) => Err(format!(
            "these codes could be CodeBreaker (12 digits each) or GameShark Advance or \
             Action Replay (16 digits each); add a device tag after the name, such as \
             \"[name] {}\" or \"[name] {}\"",
            Device::CodeBreaker.tag(),
            Device::GameSharkAdvance.tag()
        )),
        (false, false) => Err(
            "not a CodeBreaker code (XXXXXXXX YYYY), a GameShark Advance or Action Replay \
             code (XXXXXXXX YYYYYYYY) or a raw ADDRESS:VALUE patch"
                .to_owned(),
        ),
    }
}

/// The 16-digit GBA devices, in the order ties are reported.
pub(crate) const GBA16: [Device; 4] = [
    Device::GameSharkAdvance,
    Device::ProActionReplay,
    Device::GameSharkAdvanceRaw,
    Device::ProActionReplayRaw,
];

/// Picks the best-scoring reading. Readings that tie are fine if `same`
/// says they do the same thing.
///
/// # Errors
///
/// Returns why no reading is plausible, or which readings tie.
pub(crate) fn choose_gba16(
    scores: &[(Device, i32)],
    same: impl Fn(Device, Device) -> bool,
) -> Result<Device, String> {
    let best = scores.iter().map(|&(_, score)| score).max().unwrap_or(0);
    if best <= 0 {
        return Err(format!(
            "these codes don't look like GameShark Advance or Pro Action Replay v3 codes, \
             encrypted or not; check them, or add a device tag to read them as one ({})",
            GBA16.map(Device::tag).join(", ")
        ));
    }
    let tied: Vec<Device> = scores
        .iter()
        .filter(|&&(_, score)| score == best)
        .map(|&(device, _)| device)
        .collect();
    let first = tied[0];
    match tied[1..].iter().find(|&&other| !same(first, other)) {
        None => Ok(first),
        Some(other) => Err(format!(
            "these codes read as both {first} and {other}; add a device tag after the \
             name, such as \"[name] {}\" or \"[name] {}\"",
            first.tag(),
            other.tag()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guess_lines(system: System, lines: &[&str]) -> Result<Guess, (usize, String)> {
        let numbered: Vec<_> = (1..).zip(lines.iter().copied()).collect();
        guess(system, &numbered)
    }

    #[test]
    fn shapes_pick_devices() {
        use Guess::{Device as One, Gba16};
        assert_eq!(
            guess_lines(System::Gba, &["02000000:01"]),
            Ok(One(Device::Raw))
        );
        assert_eq!(
            guess_lines(System::Gb, &["010138CD"]),
            Ok(One(Device::GameShark))
        );
        assert_eq!(
            guess_lines(System::Gb, &["00A-17B-C49"]),
            Ok(One(Device::GameGenie))
        );
        assert_eq!(
            guess_lines(System::Nds, &["94000130 FCFF0000"]),
            Ok(One(Device::ActionReplayDs))
        );
        assert_eq!(
            guess_lines(System::Gba, &["82025E30 270F", "02000000:01"]),
            Ok(One(Device::CodeBreaker))
        );
        assert_eq!(
            guess_lines(System::Gba, &["82025E30", "270F"]),
            Ok(One(Device::CodeBreaker))
        );
        assert_eq!(guess_lines(System::Gba, &["1A2B3C4D 5E6F7A8B"]), Ok(Gba16));
    }

    #[test]
    fn unclear_gba_shapes_ask_for_a_tag() {
        let lines = ["82025E30 270F 82025E32 0001 82025E34 0002 82025E36 0003"];
        let (line, message) = guess_lines(System::Gba, &lines).expect_err("48 digits");
        assert_eq!(line, 1);
        assert!(message.contains("device tag"), "{message}");
        assert!(guess_lines(System::Gba, &["1234"]).is_err());
        assert_eq!(
            guess_lines(System::Gba, &["", "x"]).expect_err("not hex").0,
            2
        );
    }

    #[test]
    fn ties_need_a_tag_unless_they_agree() {
        let scores = [
            (Device::GameSharkAdvanceRaw, 8),
            (Device::ProActionReplayRaw, 8),
            (Device::GameSharkAdvance, -8),
        ];
        let error = choose_gba16(&scores, |_, _| false).expect_err("tie");
        assert!(
            error.contains("gsa-raw") && error.contains("par3-raw"),
            "{error}"
        );
        assert_eq!(
            choose_gba16(&scores, |_, _| true),
            Ok(Device::GameSharkAdvanceRaw)
        );
        assert!(choose_gba16(&[(Device::GameSharkAdvance, -3)], |_, _| true).is_err());
    }
}
