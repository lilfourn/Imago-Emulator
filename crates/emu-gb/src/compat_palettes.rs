//! The colours a Game Boy Color gives games made for the original Game
//! Boy (Pan Docs, "Power Up Sequence", "Compatibility palettes").
//!
//! The CGB boot ROM sums the 16 bytes of the title of a game Nintendo
//! published, looks the sum up in a table (using the title's fourth letter
//! where several titles share a sum), and loads a combination of three
//! palettes: one for the background and one for each object palette. Other
//! games get combination 0. Holding one of twelve button combinations as
//! the console starts picks a combination instead.
//!
//! The tables are those of SameBoy's open-source CGB boot ROM
//! (`BootROMs/cgb_boot.asm`), which reproduces the original's, used under
//! the Expat (MIT) License below. SameBoy's own extra palettes are left out.

// Copyright (c) 2015-2026 Lior Halphon
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to permit
// persons to whom the Software is furnished to do so, subject to the
// following conditions:
//
// The above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
// NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
// DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
// OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
// USE OR OTHER DEALINGS IN THE SOFTWARE.

use emu_core::DmgColors;

/// Title checksums of the games the boot ROM knows. Past
/// [`FIRST_SHARED_CHECKSUM`], several titles share a sum and the
/// fourth letter of the title in [`FOURTH_LETTERS`] tells them apart.
const TITLE_CHECKSUMS: [u8; 94] = [
    0x00, 0x88, 0x16, 0x36, 0xD1, 0xDB, 0xF2, 0x3C, 0x8C, 0x92, 0x3D, 0x5C, 0x58, 0xC9, 0x3E, 0x70,
    0x1D, 0x59, 0x69, 0x19, 0x35, 0xA8, 0x14, 0xAA, 0x75, 0x95, 0x99, 0x34, 0x6F, 0x15, 0xFF, 0x97,
    0x4B, 0x90, 0x17, 0x10, 0x39, 0xF7, 0xF6, 0xA2, 0x49, 0x4E, 0x43, 0x68, 0xE0, 0x8B, 0xF0, 0xCE,
    0x0C, 0x29, 0xE8, 0xB7, 0x86, 0x9A, 0x52, 0x01, 0x9D, 0x71, 0x9C, 0xBD, 0x5D, 0x6D, 0x67, 0x3F,
    0x6B, 0xB3, 0x46, 0x28, 0xA5, 0xC6, 0xD3, 0x27, 0x61, 0x18, 0x66, 0x6A, 0xBF, 0x0D, 0xF4, 0xB3,
    0x46, 0x28, 0xA5, 0xC6, 0xD3, 0x27, 0x61, 0x18, 0x66, 0x6A, 0xBF, 0x0D, 0xF4, 0xB3,
];
const FIRST_SHARED_CHECKSUM: usize = 65;
const FOURTH_LETTERS: &[u8; 29] = b"BEFAARBEKEK R-URAR INAILICE R";
/// The palette combination of each entry of [`TITLE_CHECKSUMS`].
const COMBINATION_PER_CHECKSUM: [u8; 94] = [
    0, 4, 5, 35, 34, 3, 31, 15, 10, 5, 19, 36, 7, 37, 30, 44, 21, 32, 31, 20, 5, 33, 13, 14, 5, 29,
    5, 18, 9, 3, 2, 26, 25, 25, 41, 42, 26, 45, 42, 45, 36, 38, 26, 42, 30, 41, 34, 34, 5, 42, 6,
    5, 33, 25, 42, 42, 40, 2, 16, 25, 42, 42, 5, 0, 39, 36, 22, 25, 6, 32, 12, 36, 11, 39, 18, 39,
    24, 31, 50, 17, 46, 6, 27, 0, 47, 41, 41, 0, 0, 19, 34, 23, 18, 29,
];
/// Palette combinations: where the OBJ 0, OBJ 1 and BG palettes start in
/// [`COLORS`]. Most start on a palette boundary; a few straddle two.
const COMBINATIONS: [[u8; 3]; 51] = [
    [16, 16, 116],   // 0, Right + A
    [72, 72, 72],    // 1, Right
    [80, 80, 80],    // 2
    [96, 96, 96],    // 3, Down + A
    [36, 36, 36],    // 4
    [0, 0, 0],       // 5, Up
    [108, 108, 108], // 6, Right + B
    [20, 20, 20],    // 7, Left + B
    [48, 48, 48],    // 8, Down
    [104, 104, 104], // 9
    [64, 32, 32],    // 10
    [16, 112, 112],  // 11
    [16, 8, 8],      // 12
    [12, 16, 16],    // 13
    [16, 116, 116],  // 14
    [112, 16, 112],  // 15
    [8, 68, 8],      // 16
    [64, 64, 32],    // 17
    [16, 16, 28],    // 18
    [16, 16, 72],    // 19
    [16, 16, 80],    // 20
    [76, 76, 36],    // 21
    [15, 15, 44],    // 22
    [68, 68, 8],     // 23
    [16, 16, 8],     // 24
    [16, 16, 12],    // 25
    [112, 112, 0],   // 26
    [12, 12, 0],     // 27
    [0, 0, 4],       // 28, Up + B
    [72, 88, 72],    // 29
    [80, 88, 80],    // 30
    [96, 88, 96],    // 31
    [64, 88, 32],    // 32
    [68, 16, 52],    // 33
    [111, 0, 56],    // 34
    [111, 16, 60],   // 35
    [76, 91, 36],    // 36
    [64, 112, 40],   // 37
    [16, 92, 112],   // 38
    [68, 88, 8],     // 39
    [16, 0, 8],      // 40, Left + A
    [16, 112, 12],   // 41
    [112, 12, 0],    // 42
    [12, 112, 16],   // 43, Up + A
    [84, 112, 16],   // 44
    [12, 112, 0],    // 45
    [100, 12, 112],  // 46
    [0, 112, 32],    // 47
    [16, 12, 112],   // 48, Left
    [112, 12, 24],   // 49, Down + B
    [16, 112, 116],  // 50
];
/// Thirty palettes of four BGR555 colours, for shades 0 to 3.
const COLORS: [u16; 120] = [
    0x7FFF, 0x32BF, 0x00D0, 0x0000, // 0
    0x639F, 0x4279, 0x15B0, 0x04CB, // 1
    0x7FFF, 0x6E31, 0x454A, 0x0000, // 2
    0x7FFF, 0x1BEF, 0x0200, 0x0000, // 3
    0x7FFF, 0x421F, 0x1CF2, 0x0000, // 4
    0x7FFF, 0x5294, 0x294A, 0x0000, // 5
    0x7FFF, 0x03FF, 0x012F, 0x0000, // 6
    0x7FFF, 0x03EF, 0x01D6, 0x0000, // 7
    0x7FFF, 0x42B5, 0x3DC8, 0x0000, // 8
    0x7E74, 0x03FF, 0x0180, 0x0000, // 9
    0x67FF, 0x77AC, 0x1A13, 0x2D6B, // 10
    0x7ED6, 0x4BFF, 0x2175, 0x0000, // 11
    0x53FF, 0x4A5F, 0x7E52, 0x0000, // 12
    0x4FFF, 0x7ED2, 0x3A4C, 0x1CE0, // 13
    0x03ED, 0x7FFF, 0x255F, 0x0000, // 14
    0x036A, 0x021F, 0x03FF, 0x7FFF, // 15
    0x7FFF, 0x01DF, 0x0112, 0x0000, // 16
    0x231F, 0x035F, 0x00F2, 0x0009, // 17
    0x7FFF, 0x03EA, 0x011F, 0x0000, // 18
    0x299F, 0x001A, 0x000C, 0x0000, // 19
    0x7FFF, 0x027F, 0x001F, 0x0000, // 20
    0x7FFF, 0x03E0, 0x0206, 0x0120, // 21
    0x7FFF, 0x7EEB, 0x001F, 0x7C00, // 22
    0x7FFF, 0x3FFF, 0x7E00, 0x001F, // 23
    0x7FFF, 0x03FF, 0x001F, 0x0000, // 24
    0x03FF, 0x001F, 0x000C, 0x0000, // 25
    0x7FFF, 0x033F, 0x0193, 0x0000, // 26
    0x0000, 0x4200, 0x037F, 0x7FFF, // 27
    0x7FFF, 0x7E8C, 0x7C00, 0x0000, // 28
    0x7FFF, 0x1BEF, 0x6180, 0x0000, // 29
];
/// The combination each held button combination picks, in the order of
/// [`DmgColors::Buttons`].
const BUTTON_COMBINATIONS: [u8; 12] = [1, 48, 5, 8, 0, 40, 43, 3, 6, 7, 28, 49];

/// The three palettes the boot ROM loads, as four BGR555 colours each,
/// for shades 0 to 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Palettes {
    /// Background palette 0, which BGP's shades index.
    pub(crate) background: [u16; 4],
    /// Object palettes 0 and 1, which OBP0's and OBP1's shades index.
    pub(crate) objects: [[u16; 4]; 2],
}

/// The sum of the 16 title bytes in `rom`'s header, if Nintendo published
/// the game: its old licensee code (0x014B) is 0x01, or 0x33 with the new
/// licensee code (0x0144) "01". The boot ROM leaves this in register B.
pub(crate) fn nintendo_title_checksum(rom: &[u8]) -> Option<u8> {
    let nintendo = match rom.get(0x14B) {
        Some(0x01) => true,
        Some(0x33) => rom.get(0x144..0x146) == Some(b"01"),
        _ => false,
    };
    let title = rom.get(0x134..0x144).filter(|_| nintendo)?;
    Some(
        title
            .iter()
            .fold(0, |sum: u8, byte| sum.wrapping_add(*byte)),
    )
}

/// The palette combination the boot ROM picks for `rom` by its title.
fn combination_by_title(rom: &[u8]) -> usize {
    let Some(checksum) = nintendo_title_checksum(rom) else {
        return 0;
    };
    let fourth_letter = rom.get(0x137).copied().unwrap_or(0);
    TITLE_CHECKSUMS
        .iter()
        .enumerate()
        .find(|&(index, sum)| {
            *sum == checksum
                && index
                    .checked_sub(FIRST_SHARED_CHECKSUM)
                    .is_none_or(|shared| FOURTH_LETTERS[shared] == fourth_letter)
        })
        .map_or(0, |(index, _)| usize::from(COMBINATION_PER_CHECKSUM[index]))
}

/// The palettes a Game Boy Color gives `rom`, a game made for the original
/// Game Boy, when `colors` puts it on one; `None` for [`DmgColors::Grey`].
pub(crate) fn palettes_for(rom: &[u8], colors: DmgColors) -> Option<Palettes> {
    let combination = match colors {
        DmgColors::Grey => return None,
        DmgColors::ByTitle => combination_by_title(rom),
        DmgColors::Buttons(buttons) => {
            usize::from(BUTTON_COMBINATIONS[usize::from(buttons).min(11)])
        }
    };
    let palette = |start: u8| {
        let start = usize::from(start);
        [0, 1, 2, 3].map(|color| COLORS[start + color])
    };
    let [object_0, object_1, background] = COMBINATIONS[combination];
    Some(Palettes {
        background: palette(background),
        objects: [palette(object_0), palette(object_1)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header with `title`, published by Nintendo if `nintendo`.
    fn header(title: &[u8], nintendo: bool) -> Vec<u8> {
        let mut rom = vec![0; 0x150];
        rom[0x134..0x134 + title.len()].copy_from_slice(title);
        rom[0x14B] = if nintendo { 0x01 } else { 0x00 };
        rom
    }

    #[test]
    fn the_checksum_needs_a_nintendo_licensee() {
        let mut rom = header(b"POKEMON RED", true);
        assert_eq!(nintendo_title_checksum(&rom), Some(0x14));
        rom[0x14B] = 0x33;
        assert_eq!(nintendo_title_checksum(&rom), None);
        rom[0x144..0x146].copy_from_slice(b"01");
        assert_eq!(nintendo_title_checksum(&rom), Some(0x14));
        assert_eq!(
            nintendo_title_checksum(&header(b"POKEMON RED", false)),
            None
        );
    }

    #[test]
    fn pokemon_red_is_red_and_blue_is_blue() {
        let red = palettes_for(&header(b"POKEMON RED", true), DmgColors::ByTitle);
        let blue = palettes_for(&header(b"POKEMON BLUE", true), DmgColors::ByTitle);
        // Combination 13 and, by the fourth letter, 11.
        assert_eq!(
            red.map(|palettes| palettes.background),
            Some([0x7FFF, 0x421F, 0x1CF2, 0x0000])
        );
        assert_eq!(
            blue.map(|palettes| palettes.background),
            Some([0x7FFF, 0x7E8C, 0x7C00, 0x0000])
        );
        // Red's player sprite is green, Blue's red.
        assert_eq!(red.map(|palettes| palettes.objects[0][1]), Some(0x1BEF));
        assert_eq!(blue.map(|palettes| palettes.objects[0][1]), Some(0x421F));
    }

    #[test]
    fn a_shared_checksum_with_another_fourth_letter_gets_the_default() {
        // "POKXMON BLUE" sums like "POKEMON BLUE" plus 0x13, so change a
        // later letter to bring the sum back to 0x61.
        let mut rom = header(b"POKXMON BLUE", true);
        rom[0x13F] -= 0x13;
        assert_eq!(nintendo_title_checksum(&rom), Some(0x61));
        assert_eq!(combination_by_title(&rom), 0);
        // "VEGAS STAKES" shares 0x61 with a fourth letter of A.
        assert_eq!(combination_by_title(&header(b"VEGAS STAKES", true)), 41);
    }

    #[test]
    fn unknown_and_unlicensed_games_get_combination_0() {
        let default = palettes_for(&header(b"DMG-ACID2", false), DmgColors::ByTitle);
        assert_eq!(
            default,
            Some(Palettes {
                background: [0x7FFF, 0x1BEF, 0x6180, 0x0000],
                objects: [[0x7FFF, 0x421F, 0x1CF2, 0x0000]; 2],
            })
        );
        assert_eq!(
            palettes_for(&header(b"DMG-ACID2", false), DmgColors::Grey),
            None
        );
    }

    #[test]
    fn button_combinations_override_the_title() {
        let rom = header(b"POKEMON RED", true);
        // Left + B: combination 7, palette 5 everywhere, a grey-brown.
        let left_b = palettes_for(&rom, DmgColors::Buttons(9));
        assert_eq!(
            left_b.map(|palettes| palettes.background),
            Some([0x7FFF, 0x5294, 0x294A, 0x0000])
        );
        // Out of range picks the last, Down + B.
        assert_eq!(
            palettes_for(&rom, DmgColors::Buttons(200)),
            palettes_for(&rom, DmgColors::Buttons(11))
        );
    }

    #[test]
    fn straddling_combinations_stay_inside_the_colours() {
        for combination in COMBINATIONS {
            assert!(
                combination
                    .iter()
                    .all(|start| usize::from(*start) + 4 <= COLORS.len())
            );
        }
        assert!(
            COMBINATION_PER_CHECKSUM
                .iter()
                .all(|c| usize::from(*c) < COMBINATIONS.len())
        );
    }
}
