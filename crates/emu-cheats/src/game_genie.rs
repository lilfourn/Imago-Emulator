//! Game Boy Game Genie codes, `ABC-DEF` or `ABC-DEF-GHI` (Pan Docs, "Game
//! Genie/Shark Cheats").
//!
//! The Game Genie sat between the Game Boy and the cartridge and changed
//! what the game read from ROM, not RAM. `AB` is the byte the game reads
//! instead and `FCDE` the address, its top digit XORed with F, so it lands
//! in the ROM at 0x0000–0x7FFF. The optional `GHI` holds a compare byte,
//! `GI` rotated right by two and XORed with 0xBA: the patch applies only
//! while the ROM holds that byte, which tells apart the banks that share a
//! switchable address. `H` is ignored.
//!
//! Codes are separated by spaces, line breaks or `+`, and the dashes may
//! be left out. The device took three codes at once; here there is no
//! limit.

use emu_core::RomPatch;

use crate::ops::Op;
use crate::raw;

/// Reads a cheat's lines of Game Genie codes, one operation per code. Raw
/// patches may sit among them.
///
/// # Errors
///
/// Returns the line and reason for the first code that cannot be read.
pub(crate) fn read(lines: &[(usize, &str)]) -> Result<Vec<Op>, (usize, String)> {
    let mut ops = Vec::new();
    for &(line, text) in lines {
        if raw::is_patch(text) {
            ops.push(raw::patch(text).map_err(|e| (line, e))?);
            continue;
        }
        let words = text
            .split(|c: char| c.is_whitespace() || c == '+')
            .filter(|word| !word.is_empty());
        for word in words {
            ops.push(Op::PatchRom(vec![code(word).map_err(|e| (line, e))?]));
        }
    }
    Ok(ops)
}

/// One code's patch.
fn code(word: &str) -> Result<RomPatch, String> {
    let digits: String = word.chars().filter(|&c| c != '-').collect();
    let number = matches!(digits.len(), 6 | 9)
        .then(|| u64::from_str_radix(&digits, 16).ok())
        .flatten()
        .ok_or_else(|| format!("a Game Genie code is ABC-DEF or ABC-DEF-GHI, not {word:?}"))?;
    let (main, check) = if digits.len() == 9 {
        (number >> 12, Some(number & 0xFFF))
    } else {
        (number, None)
    };
    // `main` is 0xABCDEF.
    let digit = |shift: u32| (main >> shift) as u32 & 0xF;
    let address = (digit(0) ^ 0xF) << 12 | digit(12) << 8 | digit(8) << 4 | digit(4);
    if address >= 0x8000 {
        return Err(format!(
            "{word} patches {address:04X}, outside the ROM (0000-7FFF)"
        ));
    }
    let compare = check.map(|check| {
        let gi = ((check >> 4) & 0xF0 | check & 0xF) as u8;
        gi.rotate_right(2) ^ 0xBA
    });
    Ok(RomPatch {
        address,
        value: (main >> 16) as u8,
        compare,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(address: u32, value: u8, compare: Option<u8>) -> Op {
        Op::PatchRom(vec![RomPatch {
            address,
            value,
            compare,
        }])
    }

    #[test]
    fn codes_split_on_spaces_and_plus_and_dashes_are_optional() {
        let ops = read(&[(1, "00A-17B-C49 + 3E123F"), (2, "C000:01")]).expect("valid");
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[1], patch(0x0123, 0x3E, None));
        assert!(matches!(
            ops[2],
            Op::Write {
                address: 0xC000,
                ..
            }
        ));
    }

    #[test]
    fn bad_codes_name_their_line() {
        for (text, needle) in [
            ("00A-17B-C4", "ABC-DEF-GHI"),
            ("00A-17B-C49-1", "ABC-DEF-GHI"),
            ("00A-17G-C49", "ABC-DEF-GHI"),
            // F XOR F is 0: address 0A17 is fine; 7 XOR F is 8: 8A17 is not.
            ("00A-177", "outside the ROM"),
        ] {
            let error = read(&[(4, "3E1-23F"), (5, text)]).expect_err(text);
            assert_eq!(error.0, 5, "{text}");
            assert!(error.1.contains(needle), "{text}: {}", error.1);
        }
    }
}
