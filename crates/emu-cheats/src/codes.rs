//! A cheat's code lines as codes. Raw patches stay whole; hex digits are
//! cut into codes of the device's length wherever the lines break, so
//! `94000130 FCFF0000`, `94000130FCFF0000` and a code split over two lines
//! all read the same.

use crate::ops::Op;
use crate::raw;

/// One code, or one raw patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// A raw `ADDRESS:VALUE` patch.
    Patch(Op),
    /// A device code's digits as a number: `XXXXXXXX YYYYYYYY` is
    /// `0xXXXXXXXX_YYYYYYYY`.
    Code(u64),
}

/// A code line's digits without whitespace, or `None` if it has anything
/// but hex digits and whitespace.
pub(crate) fn hex_digits(line: &str) -> Option<String> {
    let digits: String = line.split_whitespace().collect();
    digits
        .chars()
        .all(|c| c.is_ascii_hexdigit())
        .then_some(digits)
}

/// Cuts `lines` into codes of `digits` hex digits each, each with the line
/// its first digit is on.
///
/// # Errors
///
/// Returns the line and reason for a line that is neither a raw patch nor
/// hex digits, or a code left short by a patch or by the end.
pub(crate) fn items(
    lines: &[(usize, &str)],
    digits: usize,
) -> Result<Vec<(usize, Item)>, (usize, String)> {
    let short = |count: usize| format!("a code has {digits} hex digits; this one has {count}");
    let mut items = Vec::new();
    let mut pending = String::new();
    let mut pending_line = 0;
    for &(line, text) in lines {
        if raw::is_patch(text) {
            if !pending.is_empty() {
                return Err((pending_line, short(pending.len())));
            }
            items.push((line, Item::Patch(raw::patch(text).map_err(|e| (line, e))?)));
            continue;
        }
        let hex = hex_digits(text).ok_or_else(|| (line, format!("not a code: {text:?}")))?;
        for digit in hex.chars() {
            if pending.is_empty() {
                pending_line = line;
            }
            pending.push(digit);
            if pending.len() == digits {
                let code = u64::from_str_radix(&pending, 16).map_err(|_| (line, short(digits)))?;
                items.push((pending_line, Item::Code(code)));
                pending.clear();
            }
        }
    }
    if pending.is_empty() {
        Ok(items)
    } else {
        Err((pending_line, short(pending.len())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Width;

    #[test]
    fn spacing_and_line_breaks_do_not_matter() {
        let lines = [
            (1, "94000130 FCFF0000"),
            (2, "62111938"),
            (3, "00000000B2111938 00000000"),
        ];
        assert_eq!(
            items(&lines, 16),
            Ok(vec![
                (1, Item::Code(0x9400_0130_FCFF_0000)),
                (2, Item::Code(0x6211_1938_0000_0000)),
                (3, Item::Code(0xB211_1938_0000_0000)),
            ])
        );
    }

    #[test]
    fn patches_stay_in_place_and_short_codes_are_errors() {
        let lines = [
            (1, "82025E30 270F"),
            (2, "02000000:FF"),
            (3, "32025E32 0001"),
        ];
        let read = items(&lines, 12).expect("valid");
        assert_eq!(
            read[1],
            (
                2,
                Item::Patch(Op::Write {
                    address: 0x0200_0000,
                    value: 0xFF,
                    width: Width::Byte
                })
            )
        );
        assert_eq!(read.len(), 3);
        let error = items(&[(4, "82025E30"), (5, "02000000:FF")], 12).expect_err("short");
        assert_eq!(error.0, 4);
        assert!(error.1.contains("this one has 8"), "{}", error.1);
        assert_eq!(items(&[(7, "zz")], 8).expect_err("not hex").0, 7);
    }
}
