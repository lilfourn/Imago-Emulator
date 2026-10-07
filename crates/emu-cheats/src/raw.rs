//! Raw memory patches, `ADDRESS:VALUE` in hex, on every system.

use crate::ops::{Op, Width};

/// Whether `line` is a raw patch rather than a device code.
pub(crate) fn is_patch(line: &str) -> bool {
    line.contains(':')
}

/// Reads `ADDRESS:VALUE`, which writes as many bytes as the value's digits
/// need, little-endian.
pub(crate) fn patch(line: &str) -> Result<Op, String> {
    let (address, value) = line
        .split_once(':')
        .ok_or_else(|| format!("not an ADDRESS:VALUE patch: {line:?}"))?;
    let (address, value) = (address.trim(), value.trim());
    let width = match value.len() {
        1..=2 => Width::Byte,
        3..=4 => Width::Half,
        5..=8 => Width::Word,
        _ => return Err(format!("a value has 1 to 8 hex digits: {value:?}")),
    };
    Ok(Op::Write {
        address: hex(address)?,
        value: hex(value)?,
        width,
    })
}

fn hex(digits: &str) -> Result<u32, String> {
    u32::from_str_radix(digits, 16).map_err(|_| format!("not a hex number: {digits:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_sets_the_width() {
        assert_eq!(
            patch("C0A2:63"),
            Ok(Op::Write {
                address: 0xC0A2,
                value: 0x63,
                width: Width::Byte
            })
        );
        assert_eq!(
            patch("02000000 : 12345678"),
            Ok(Op::Write {
                address: 0x0200_0000,
                value: 0x1234_5678,
                width: Width::Word
            })
        );
        assert!(patch("02000000:123456789").is_err());
        assert!(patch("0200000G:12").is_err());
    }
}
