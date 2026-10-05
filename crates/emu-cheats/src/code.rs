//! Single codes in each supported format.

use emu_core::System;

use crate::{Modify, Op, Test, Width};

/// Reads one code. `Ok(None)` is a code with nothing to do in an emulator
/// (a CodeBreaker master code).
pub(crate) fn parse(system: System, line: &str) -> Result<Option<Op>, String> {
    if let Some((address, value)) = line.split_once(':') {
        return raw(address.trim(), value.trim()).map(Some);
    }
    if line.contains('-') && system == System::Gb {
        return Err("Game Genie codes patch the ROM and are not supported yet".to_owned());
    }
    let digits: String = line.split_whitespace().collect();
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("not a code: {line:?}"));
    }
    match (system, digits.len()) {
        (System::Gb, 8) => game_shark(&digits).map(Some),
        (System::Gba, 12) => code_breaker(&digits),
        (System::Gba, 16) => Err(
            "Action Replay and GameShark Advance codes are encrypted and not supported yet; \
             use CodeBreaker or raw ADDRESS:VALUE codes"
                .to_owned(),
        ),
        (System::Nds, 16) => Err("Action Replay DS codes are not supported yet".to_owned()),
        _ => Err(format!(
            "not a {} code: {line:?}",
            match system {
                System::Gb => "GameShark (01VVAAAA) or raw ADDRESS:VALUE",
                System::Gba => "CodeBreaker (XAAAAAAA YYYY) or raw ADDRESS:VALUE",
                System::Nds => "raw ADDRESS:VALUE",
            }
        )),
    }
}

fn hex(digits: &str) -> Result<u32, String> {
    u32::from_str_radix(digits, 16).map_err(|_| format!("not a hex number: {digits:?}"))
}

/// `ADDRESS:VALUE`, writing as many bytes as the value's digits need.
fn raw(address: &str, value: &str) -> Result<Op, String> {
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

/// Game Boy GameShark `TTVVAAAA`: type 01 writes byte `VV` to the address
/// `AAAA`, which is stored low byte first.
fn game_shark(digits: &str) -> Result<Op, String> {
    let code = hex(digits)?;
    let kind = code >> 24;
    if kind != 0x01 {
        return Err(format!(
            "GameShark code type {kind:02X} is not supported yet (only 01)"
        ));
    }
    let address = (code & 0xFF) << 8 | (code >> 8) & 0xFF;
    Ok(Op::Write {
        address,
        value: (code >> 16) & 0xFF,
        width: Width::Byte,
    })
}

/// Game Boy Advance CodeBreaker `TAAAAAAA YYYY`: type `T` acts on the
/// 28-bit address `AAAAAAA` with the 16-bit value `YYYY`.
fn code_breaker(digits: &str) -> Result<Option<Op>, String> {
    let kind = hex(&digits[..1])?;
    let address = hex(&digits[1..8])?;
    let value = hex(&digits[8..])?;
    let modify = |with| {
        Some(Op::Modify {
            address,
            value,
            width: Width::Half,
            with,
        })
    };
    let test = |test| {
        Some(Op::If {
            address,
            value,
            width: Width::Half,
            test,
        })
    };
    Ok(match kind {
        // Master codes tell a real cheat device where to hook the game.
        0x0 | 0x1 => None,
        0x2 => modify(Modify::Or),
        0x3 => Some(Op::Write {
            address,
            value: value & 0xFF,
            width: Width::Byte,
        }),
        0x6 => modify(Modify::And),
        0x7 => test(Test::Equal),
        0x8 => Some(Op::Write {
            address,
            value,
            width: Width::Half,
        }),
        0x9 => return Err("encrypted CodeBreaker codes are not supported yet".to_owned()),
        0xA => test(Test::NotEqual),
        0xE => modify(Modify::Add),
        _ => {
            return Err(format!(
                "CodeBreaker code type {kind:X} is not supported yet"
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_codes_take_their_width_from_the_value() {
        assert_eq!(
            parse(System::Gb, "C0A2:63"),
            Ok(Some(Op::Write {
                address: 0xC0A2,
                value: 0x63,
                width: Width::Byte
            }))
        );
        assert_eq!(
            parse(System::Nds, "02000000:12345678"),
            Ok(Some(Op::Write {
                address: 0x0200_0000,
                value: 0x1234_5678,
                width: Width::Word
            }))
        );
    }

    #[test]
    fn game_shark_addresses_are_low_byte_first() {
        assert_eq!(
            parse(System::Gb, "010138CD"),
            Ok(Some(Op::Write {
                address: 0xCD38,
                value: 0x01,
                width: Width::Byte
            }))
        );
    }

    #[test]
    fn code_breaker_types() {
        assert_eq!(
            parse(System::Gba, "32025E30 0063"),
            Ok(Some(Op::Write {
                address: 0x0202_5E30,
                value: 0x63,
                width: Width::Byte
            }))
        );
        assert_eq!(parse(System::Gba, "0000A1B2 000A"), Ok(None));
        assert!(parse(System::Gba, "9123ABCD 0001").is_err());
    }

    #[test]
    fn unsupported_formats_say_what_they_are() {
        let error = parse(System::Gba, "1A2B3C4D 5E6F7A8B").expect_err("encrypted");
        assert!(error.contains("encrypted"), "{error}");
        let error = parse(System::Gb, "00A-17B-C49").expect_err("Game Genie");
        assert!(error.contains("Game Genie"), "{error}");
    }
}
