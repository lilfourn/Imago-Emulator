//! What each I/O register does with a write of each width: a byte, a
//! halfword and a word. Several registers have rules of their own (IF
//! clears only the ones written, POSTFLG and HALTCNT are bytes, VRAMCNT
//! is a byte a bank, the IPC FIFO takes only words), and the raw copy of
//! what was written, which a save state holds, records some writes and
//! not others. These tests pin all of it.

use emu_state::payload;
use serde::Serialize;

use crate::hardware::{Event, Hardware};
use crate::interrupt::irq;
use crate::ipc::Side;

const SIDES: [Side; 2] = [Side::Arm9, Side::Arm7];

fn hardware() -> Hardware {
    Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into())
}

/// `side` writes the low `bytes` bytes (1, 2 or 4) of `value` to
/// `address`.
fn write(hw: &mut Hardware, side: Side, bytes: u32, address: u32, value: u32) {
    match (side, bytes) {
        (Side::Arm9, 1) => hw.arm9_write_8(address, value as u8),
        (Side::Arm9, 2) => hw.arm9_write_16(address, value as u16),
        (Side::Arm9, _) => hw.arm9_write_32(address, value),
        (Side::Arm7, 1) => hw.arm7_write_8(address, value as u8),
        (Side::Arm7, 2) => hw.arm7_write_16(address, value as u16),
        (Side::Arm7, _) => hw.arm7_write_32(address, value),
    }
}

/// `side` reads `bytes` bytes (1, 2 or 4) at `address`.
fn read(hw: &mut Hardware, side: Side, bytes: u32, address: u32) -> u32 {
    match (side, bytes) {
        (Side::Arm9, 1) => u32::from(hw.arm9_read_8(address)),
        (Side::Arm9, 2) => u32::from(hw.arm9_read_16(address)),
        (Side::Arm9, _) => hw.arm9_read_32(address),
        (Side::Arm7, 1) => u32::from(hw.arm7_read_8(address)),
        (Side::Arm7, 2) => u32::from(hw.arm7_read_16(address)),
        (Side::Arm7, _) => hw.arm7_read_32(address),
    }
}

/// The word at `address` as `side` reads it, which must be the same read
/// as a word, as two halfwords and as four bytes.
fn read_word(hw: &mut Hardware, side: Side, address: u32) -> u32 {
    let word = read(hw, side, 4, address);
    let halves = read(hw, side, 2, address) | read(hw, side, 2, address + 2) << 16;
    let bytes = (0..4).fold(0, |all, i| all | read(hw, side, 1, address + i) << (i * 8));
    assert_eq!(halves, word, "{address:#010X} in halfwords");
    assert_eq!(bytes, word, "{address:#010X} in bytes");
    word
}

/// The raw copy's word at `offset` in `side`'s page: what a save state
/// holds there.
fn raw(hw: &Hardware, side: Side, offset: usize) -> u32 {
    let page = match side {
        Side::Arm9 => &hw.io9,
        Side::Arm7 => &hw.io7,
    };
    u32::from_le_bytes([
        page[offset],
        page[offset + 1],
        page[offset + 2],
        page[offset + 3],
    ])
}

/// A part of the hardware as a save state holds it, for parts whose
/// registers cannot be read back.
fn saved<T: Serialize>(part: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    payload::append(part, &mut bytes);
    bytes
}

#[test]
fn interrupt_flags_clear_only_the_ones_written_at_every_width() {
    for side in SIDES {
        for byte in 0..4 {
            let mut hw = hardware();
            hw.cpu_mut(side).interrupts.flags = u32::MAX;
            write(&mut hw, side, 1, 0x0400_0214 + byte, 0xA5);
            assert_eq!(hw.cpu(side).interrupts.flags, !(0xA5 << (byte * 8)));
            assert_eq!(raw(&hw, side, 0x214), 0, "a byte to IF is not kept");
        }
        for half in 0..2 {
            let mut hw = hardware();
            hw.cpu_mut(side).interrupts.flags = u32::MAX;
            write(&mut hw, side, 2, 0x0400_0214 + half * 2, 0x8001);
            assert_eq!(hw.cpu(side).interrupts.flags, !(0x8001 << (half * 16)));
            assert_eq!(raw(&hw, side, 0x214), 0x8001 << (half * 16));
        }
        let mut hw = hardware();
        hw.cpu_mut(side).interrupts.flags = u32::MAX;
        write(&mut hw, side, 4, 0x0400_0214, 0x0101_0180);
        assert_eq!(hw.cpu(side).interrupts.flags, !0x0101_0180);
        assert_eq!(raw(&hw, side, 0x214), 0x0101_0180);
        // Zeros clear nothing, and IF reads the same at every width.
        write(&mut hw, side, 4, 0x0400_0214, 0);
        assert_eq!(read_word(&mut hw, side, 0x0400_0214), !0x0101_0180);
    }
}

#[test]
fn interrupt_enables_take_bytes_halfwords_and_words() {
    for side in SIDES {
        let mut hw = hardware();
        write(&mut hw, side, 4, 0x0400_0210, 0x1234_5678);
        assert_eq!(hw.cpu(side).interrupts.enable, 0x1234_5678);
        write(&mut hw, side, 2, 0x0400_0212, 0xABCD);
        assert_eq!(hw.cpu(side).interrupts.enable, 0xABCD_5678);
        write(&mut hw, side, 2, 0x0400_0210, 0x0F0F);
        assert_eq!(hw.cpu(side).interrupts.enable, 0xABCD_0F0F);
        for (byte, value) in [(0, 0x11), (1, 0x22), (2, 0x33), (3, 0x44)] {
            write(&mut hw, side, 1, 0x0400_0210 + byte, value);
        }
        assert_eq!(hw.cpu(side).interrupts.enable, 0x4433_2211);
        assert_eq!(read_word(&mut hw, side, 0x0400_0210), 0x4433_2211);
        assert_eq!(raw(&hw, side, 0x210), 0x4433_2211);
    }
}

#[test]
fn the_master_enable_is_bit_0_of_its_low_byte() {
    for side in SIDES {
        let mut hw = hardware();
        write(&mut hw, side, 2, 0x0400_0208, 0xFFFF);
        assert!(hw.cpu(side).interrupts.master_enable);
        // The byte above it holds nothing.
        write(&mut hw, side, 1, 0x0400_0209, 0x00);
        assert!(hw.cpu(side).interrupts.master_enable);
        write(&mut hw, side, 1, 0x0400_0208, 0xFE);
        assert!(!hw.cpu(side).interrupts.master_enable);
        write(&mut hw, side, 1, 0x0400_0209, 0xFF);
        assert!(!hw.cpu(side).interrupts.master_enable);
        write(&mut hw, side, 4, 0x0400_0208, 0xFFFF_FFFF);
        assert!(hw.cpu(side).interrupts.master_enable);
        // IME reads as 0 or 1; the halfword beside it is no register and
        // reads back what was written.
        assert_eq!(read_word(&mut hw, side, 0x0400_0208), 0xFFFF_0001);
        assert_eq!(raw(&hw, side, 0x208), 0xFFFF_FFFF);
    }
}

#[test]
fn the_arm9s_postflg_is_two_bits_of_one_byte() {
    let mut hw = hardware();
    hw.arm9_write_8(0x0400_0300, 0xFF);
    assert_eq!(hw.arm9.postflg, 3);
    assert_eq!(
        raw(&hw, Side::Arm9, 0x300),
        0,
        "a byte to POSTFLG is not kept"
    );
    hw.arm9_write_8(0x0400_0300, 0);
    assert_eq!(
        hw.arm9.postflg, 1,
        "bit 1 is cleared; bit 0, once set, stays"
    );
    hw.arm9_write_16(0x0400_0300, 0xFF01);
    assert_eq!(hw.arm9.postflg, 1);
    assert_eq!(raw(&hw, Side::Arm9, 0x300), 0xFF01);
    // The byte above it holds nothing.
    hw.arm9_write_8(0x0400_0301, 0x02);
    assert_eq!(hw.arm9.postflg, 1);
    assert_eq!(raw(&hw, Side::Arm9, 0x300), 0x0201);
    hw.arm9_write_32(0x0400_0300, 0xFFFF_FFFE);
    assert_eq!(hw.arm9.postflg, 3);
    assert_eq!(raw(&hw, Side::Arm9, 0x300), 0xFFFF_FFFE);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_0300), 0xFFFF_0003);
    // Before boot has ended, bit 0 is still to be set.
    let mut booting = hardware();
    booting.arm9_write_8(0x0400_0300, 2);
    assert_eq!(booting.arm9.postflg, 2);
    booting.arm9_write_8(0x0400_0300, 0);
    assert_eq!(booting.arm9.postflg, 0);
    assert_eq!(hw.arm7.postflg, 0, "each CPU has its own");
}

#[test]
fn the_arm7s_postflg_can_only_be_set_and_haltcnt_is_the_byte_above_it() {
    for bytes in [1, 2, 4] {
        let mut hw = hardware();
        write(&mut hw, Side::Arm7, bytes, 0x0400_0300, 0xFFFF_0001);
        assert_eq!(hw.arm7.postflg, 1, "{bytes} bytes");
        assert!(!hw.arm7.halted, "HALTCNT 0 does not halt");
        write(&mut hw, Side::Arm7, bytes, 0x0400_0300, 0);
        assert_eq!(hw.arm7.postflg, 1, "it cannot be cleared");
        // Neither byte is ever kept.
        assert_eq!(raw(&hw, Side::Arm7, 0x300), 0, "{bytes} bytes");
    }
    let mut hw = hardware();
    hw.arm7_write_32(0x0400_0300, 0xABCD_0000);
    assert_eq!(raw(&hw, Side::Arm7, 0x300), 0xABCD_0000);
    assert_eq!(hw.arm7_read_32(0x0400_0300), 0xABCD_0000);

    // HALTCNT: 2 halts and 3 sleeps; 0 and 1 do not.
    for (value, halts) in [(0x00, false), (0x40, false), (0x80, true), (0xC0, true)] {
        let mut hw = hardware();
        hw.arm7_write_8(0x0400_0301, value);
        assert_eq!(hw.arm7.halted, halts, "byte {value:#X}");
        let mut hw = hardware();
        hw.arm7_write_16(0x0400_0300, u16::from(value) << 8);
        assert_eq!(hw.arm7.halted, halts, "halfword {value:#X}");
        assert_eq!(hw.arm7.postflg, 0);
        let mut hw = hardware();
        hw.arm7_write_32(0x0400_0300, u32::from(value) << 8 | 1);
        assert_eq!(hw.arm7.halted, halts, "word {value:#X}");
        assert_eq!(hw.arm7.postflg, 1);
    }
    // The ARM9 has no HALTCNT.
    let mut hw = hardware();
    hw.arm9_write_8(0x0400_0301, 0x80);
    hw.arm9_write_16(0x0400_0300, 0x8000);
    assert!(!hw.arm9.halted && !hw.arm7.halted);
}

#[test]
fn vramcnt_is_a_byte_a_bank_with_wramcnt_among_them() {
    // Bank n's byte: A–G at 0x240–0x246, WRAMCNT at 0x247, H and I after.
    let banks = |hw: &Hardware| -> [u8; 9] { std::array::from_fn(|bank| hw.vram.control(bank)) };
    let mut hw = hardware();
    for (bank, offset) in [
        0x240, 0x241, 0x242, 0x243, 0x244, 0x245, 0x246, 0x248, 0x249,
    ]
    .into_iter()
    .enumerate()
    {
        hw.arm9_write_8(0x0400_0000 + offset, 0xFF);
        // Bits 5 and 6 are not there.
        assert_eq!(hw.vram.control(bank), 0x9F, "bank {bank}");
        assert_eq!(hw.arm9_read_8(0x0400_0000 + offset), 0x9F);
    }
    assert_eq!(hw.wramcnt, 0);
    hw.arm9_write_8(0x0400_0247, 0xFF);
    assert_eq!(hw.wramcnt, 3);
    assert_eq!(
        raw(&hw, Side::Arm9, 0x240),
        0,
        "bytes to VRAMCNT are not kept"
    );
    assert_eq!(raw(&hw, Side::Arm9, 0x244), 0);
    assert_eq!(raw(&hw, Side::Arm9, 0x248), 0);

    // A halfword is two of them.
    let mut hw = hardware();
    hw.arm9_write_16(0x0400_0240, 0x8281);
    hw.arm9_write_16(0x0400_0246, 0x0284); // G, then WRAMCNT
    hw.arm9_write_16(0x0400_0248, 0x8183); // H and I
    assert_eq!(banks(&hw), [0x81, 0x82, 0, 0, 0, 0, 0x84, 0x83, 0x81]);
    assert_eq!(hw.wramcnt, 2);
    assert_eq!(raw(&hw, Side::Arm9, 0x240), 0x0000_8281);
    assert_eq!(raw(&hw, Side::Arm9, 0x244), 0x0284_0000);
    assert_eq!(raw(&hw, Side::Arm9, 0x248), 0x0000_8183);

    // A word is four.
    let mut hw = hardware();
    hw.arm9_write_32(0x0400_0240, 0x8483_8281);
    hw.arm9_write_32(0x0400_0244, 0x0187_8685); // E, F, G, WRAMCNT
    hw.arm9_write_32(0x0400_0248, 0xFFFF_8281); // H, I and two that are none
    let expected = [0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x81, 0x82];
    assert_eq!(banks(&hw), expected);
    assert_eq!(hw.wramcnt, 1);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_0240), 0x8483_8281);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_0244), 0x0187_8685);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_0248), 0xFFFF_8281);
    assert_eq!(raw(&hw, Side::Arm9, 0x248), 0xFFFF_8281);

    // The ARM7 reads which of C and D it has and WRAMCNT, and writes none.
    let mut hw = hardware();
    hw.arm9_write_8(0x0400_0242, 0x82);
    hw.arm9_write_8(0x0400_0247, 3);
    for bytes in [1, 2, 4] {
        write(&mut hw, Side::Arm7, bytes, 0x0400_0240, 0);
        write(&mut hw, Side::Arm7, bytes, 0x0400_0244, 0);
    }
    assert_eq!(hw.vram.control(2), 0x82);
    assert_eq!(hw.wramcnt, 3);
    assert_eq!(hw.arm7_read_16(0x0400_0240), 0x0301);
    assert_eq!(hw.arm7_read_8(0x0400_0241), 0x03);
}

#[test]
fn dispstat_keeps_its_enables_and_line_setting_for_each_cpu() {
    for side in SIDES {
        let other = SIDES[usize::from(side == Side::Arm9)];
        let mut hw = hardware();
        // Bits 0–2 are status and bit 6 is not there.
        write(&mut hw, side, 2, 0x0400_0004, 0xFFFF);
        assert_eq!(hw.cpu(side).dispstat, 0xFFB8);
        assert_eq!(hw.cpu(other).dispstat, 0, "each CPU has its own");
        assert_eq!(raw(&hw, side, 0x004), 0xFFFF);
        // A byte at a time: the enables below, the line setting above.
        write(&mut hw, side, 1, 0x0400_0004, 0x08);
        assert_eq!(hw.cpu(side).dispstat, 0xFF08);
        write(&mut hw, side, 1, 0x0400_0005, 0x12);
        assert_eq!(hw.cpu(side).dispstat, 0x1208);
        assert_eq!(raw(&hw, side, 0x004), 0x1208);
        // A word covers VCOUNT, which cannot be written.
        hw.vcount = 100;
        write(&mut hw, side, 4, 0x0400_0004, 0x00C7_34B8);
        assert_eq!(hw.cpu(side).dispstat, 0x34B8);
        assert_eq!(hw.vcount, 100);
        write(&mut hw, side, 2, 0x0400_0006, 0x0001);
        write(&mut hw, side, 1, 0x0400_0006, 0x02);
        assert_eq!(hw.vcount, 100);
        // Reading gives the stored bits and the live status: line 100 is
        // not in VBlank, and the line setting is 0x34 with bit 7 as its
        // ninth bit, which is not line 100.
        assert_eq!(read_word(&mut hw, side, 0x0400_0004), 100 << 16 | 0x34B8);
        hw.vcount = 200;
        hw.hblank = true;
        assert_eq!(read(&mut hw, side, 2, 0x0400_0004), 0x34B8 | 0b011);
    }
}

#[test]
fn timers_take_their_reload_and_control_at_every_width() {
    for side in SIDES {
        let mut hw = hardware();
        let counter = |hw: &Hardware, timer: u32| {
            let now = hw.now(side);
            hw.cpu(side).timers.read(timer * 4, now)
        };
        // Timer 1: the reload a byte at a time, then control's low byte
        // starts it from the reload. The counter is what reads back.
        hw_write_8s(&mut hw, side, 0x0400_0104, &[0x34, 0x12]);
        assert_eq!(counter(&hw, 1), 0, "not started");
        write(&mut hw, side, 1, 0x0400_0106, 0x80);
        assert_eq!(counter(&hw, 1), 0x1234);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0106), 0x0080);
        assert_eq!(raw(&hw, side, 0x104), 0x0080_1234);
        // Control's high byte holds nothing: the timer runs on.
        write(&mut hw, side, 1, 0x0400_0107, 0xFF);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0106), 0x0080);
        assert_eq!(counter(&hw, 1), 0x1234);
        // A new reload does not move a running counter.
        write(&mut hw, side, 2, 0x0400_0104, 0xFFF0);
        assert_eq!(counter(&hw, 1), 0x1234);

        // Timer 2: a word sets the reload and starts from it at once.
        // Control keeps bits 0–2, 6 and 7.
        write(&mut hw, side, 4, 0x0400_0108, 0xFFFF_ABCD);
        assert_eq!(counter(&hw, 2), 0xABCD);
        assert_eq!(read_word(&mut hw, side, 0x0400_0108), 0x00C7_ABCD);
        assert_eq!(raw(&hw, side, 0x108), 0xFFFF_ABCD);

        // Timer 3: halfwords, control then reload: it started from 0.
        write(&mut hw, side, 2, 0x0400_010E, 0x0080);
        write(&mut hw, side, 2, 0x0400_010C, 0x5555);
        assert_eq!(counter(&hw, 3), 0);
    }
}

/// `side` writes `bytes` one at a time from `address` on.
fn hw_write_8s(hw: &mut Hardware, side: Side, address: u32, bytes: &[u8]) {
    for (offset, byte) in (0u32..).zip(bytes) {
        write(hw, side, 1, address + offset, u32::from(*byte));
    }
}

#[test]
fn keycnt_is_checked_against_the_keys_whenever_it_is_written() {
    for side in SIDES {
        let mut hw = hardware();
        hw.keys = 0x03FE; // A held
        // KEYINPUT, in the same word, cannot be written.
        write(&mut hw, side, 4, 0x0400_0130, 0x0002_0000);
        assert_eq!(hw.keys, 0x03FE);
        assert_eq!(hw.cpu(side).keycnt, 0x0002);
        assert_eq!(hw.cpu(side).interrupts.flags, 0, "no interrupt asked for");
        // B selected, interrupt on: B is not held.
        write(&mut hw, side, 1, 0x0400_0133, 0x40);
        assert_eq!(hw.cpu(side).keycnt, 0x4002);
        assert_eq!(hw.cpu(side).interrupts.flags, 0);
        // A selected as well: the low byte alone raises it.
        write(&mut hw, side, 1, 0x0400_0132, 0x03);
        assert_eq!(hw.cpu(side).keycnt, 0x4003);
        assert_eq!(hw.cpu(side).interrupts.flags, irq::KEYPAD);
        hw.cpu_mut(side).interrupts.flags = 0;
        // All of the selected keys: B is missing.
        write(&mut hw, side, 2, 0x0400_0132, 0xC003);
        assert_eq!(hw.cpu(side).interrupts.flags, 0);
        assert_eq!(read_word(&mut hw, side, 0x0400_0130), 0xC003_03FE);
        assert_eq!(raw(&hw, side, 0x130), 0xC003_0000);
    }
}

#[test]
fn ipcsync_shows_the_other_cpu_its_data_and_sends_it_an_interrupt() {
    for side in SIDES {
        let other = SIDES[usize::from(side == Side::Arm9)];
        let mut hw = hardware();
        // The other CPU asks for the interrupt.
        write(&mut hw, other, 2, 0x0400_0180, 0x4000);
        // Data out is bits 8–11; the low byte is the other CPU's data.
        write(&mut hw, side, 2, 0x0400_0180, 0x0AFF);
        assert_eq!(read(&mut hw, other, 2, 0x0400_0180), 0x400A);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0180), 0x0A00);
        assert_eq!(hw.cpu(other).interrupts.flags, 0);
        // Bit 13 sends the interrupt; it does not read back.
        write(&mut hw, side, 2, 0x0400_0180, 0x2500);
        assert_eq!(hw.cpu(other).interrupts.flags, irq::IPC_SYNC);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0180), 0x0500);
        hw.cpu_mut(other).interrupts.flags = 0;
        // The high byte alone does the same.
        write(&mut hw, side, 1, 0x0400_0181, 0x63);
        assert_eq!(hw.cpu(other).interrupts.flags, irq::IPC_SYNC);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0180), 0x4300);
        assert_eq!(read(&mut hw, other, 2, 0x0400_0180), 0x4003);
        hw.cpu_mut(other).interrupts.flags = 0;
        // A word: the halfword beside IPCSYNC is no register.
        write(&mut hw, side, 4, 0x0400_0180, 0xFFFF_0C00);
        assert_eq!(hw.cpu(other).interrupts.flags, 0);
        assert_eq!(read_word(&mut hw, side, 0x0400_0180), 0xFFFF_0C00);
        // A low byte written with nothing to send changes nothing.
        write(&mut hw, side, 1, 0x0400_0180, 0xFF);
        assert_eq!(hw.cpu(other).interrupts.flags, 0);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0180), 0x0C00);
        assert_eq!(raw(&hw, side, 0x180), 0xFFFF_0CFF);
        // Without the other's bit 14, bit 13 sends nothing.
        write(&mut hw, other, 2, 0x0400_0180, 0);
        write(&mut hw, side, 2, 0x0400_0180, 0x2000);
        assert_eq!(hw.cpu(other).interrupts.flags, 0);
    }
}

#[test]
fn the_ipc_fifo_control_enables_flushes_and_acknowledges_by_the_bits_written() {
    for side in SIDES {
        let other = SIDES[usize::from(side == Side::Arm9)];
        let mut hw = hardware();
        // Enabled: empty both ways.
        write(&mut hw, side, 2, 0x0400_0184, 0x8000);
        assert_eq!(read_word(&mut hw, side, 0x0400_0184), 0x8101);
        write(&mut hw, side, 4, 0x0400_0188, 0x1111_1111);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184), 0x8100, "a word waits");
        assert_eq!(read(&mut hw, other, 2, 0x0400_0184), 0x0001);
        // The low byte: bit 3 flushes what was sent, bit 2 asks for the
        // send-empty interrupt, which fires at once on an empty FIFO.
        write(&mut hw, side, 1, 0x0400_0184, 0x0C);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184), 0x8105);
        assert_eq!(hw.cpu(side).interrupts.flags, irq::IPC_SEND_EMPTY);
        hw.cpu_mut(side).interrupts.flags = 0;
        // The high byte: bit 10 asks for the receive interrupt, which
        // fires at once when a word waits.
        write(&mut hw, other, 2, 0x0400_0184, 0x8000);
        write(&mut hw, other, 4, 0x0400_0188, 0x2222_2222);
        write(&mut hw, side, 1, 0x0400_0185, 0x84);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184), 0x8405);
        assert_eq!(hw.cpu(side).interrupts.flags, irq::IPC_RECEIVE_NOT_EMPTY);
        assert_eq!(raw(&hw, side, 0x184), 0x840C);

        // The error flag: sending to a full FIFO sets it, and writing a
        // one to bit 14 clears it; a zero there leaves it.
        let mut hw = hardware();
        write(&mut hw, side, 2, 0x0400_0184, 0x8000);
        for word in 0..17 {
            write(&mut hw, side, 4, 0x0400_0188, word);
        }
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184), 0xC102, "full, error");
        write(&mut hw, side, 2, 0x0400_0184, 0x8000);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184), 0xC102);
        write(&mut hw, side, 1, 0x0400_0185, 0xC0);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184), 0x8102);
        // A word: the halfword beside the control is no register.
        write(&mut hw, side, 4, 0x0400_0184, 0xFFFF_8008);
        assert_eq!(read_word(&mut hw, side, 0x0400_0184), 0xFFFF_8101);
    }
}

#[test]
fn the_ipc_fifo_sends_whole_words_and_a_read_of_any_width_takes_one() {
    for side in SIDES {
        let other = SIDES[usize::from(side == Side::Arm9)];
        let mut hw = hardware();
        write(&mut hw, side, 2, 0x0400_0184, 0x8000);
        write(&mut hw, other, 2, 0x0400_0184, 0x8000);
        // Halfwords and bytes to the send port send nothing; they are
        // kept, as for any offset with no register.
        write(&mut hw, side, 2, 0x0400_0188, 0x1234);
        write(&mut hw, side, 2, 0x0400_018A, 0x5678);
        write(&mut hw, side, 1, 0x0400_0188, 0x9A);
        assert_eq!(read(&mut hw, other, 2, 0x0400_0184), 0x8101, "nothing came");
        assert_eq!(raw(&hw, side, 0x188), 0x5678_129A);
        // A word sends, and is not kept.
        for word in [0xAABB_CCDD, 0x1122_3344, 0x5566_7788] {
            write(&mut hw, side, 4, 0x0400_0188, word);
        }
        assert_eq!(raw(&hw, side, 0x188), 0x5678_129A);
        // The receive port, above the page: a word, then a halfword and a
        // byte, each of which takes a whole word out.
        assert_eq!(read(&mut hw, other, 4, 0x0410_0000), 0xAABB_CCDD);
        assert_eq!(read(&mut hw, other, 2, 0x0410_0002), 0x1122);
        assert_eq!(read(&mut hw, other, 1, 0x0410_0001), 0x77);
        assert_eq!(read(&mut hw, other, 2, 0x0400_0184), 0x8101, "all taken");
        // Nothing above the page can be written.
        for bytes in [1, 2, 4] {
            write(&mut hw, other, bytes, 0x0410_0000, 0xFFFF_FFFF);
            write(&mut hw, other, bytes, 0x0410_0188, 0xFFFF_FFFF);
        }
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184), 0x8101);
    }
}

#[test]
fn the_game_cards_registers_belong_to_the_cpu_exmemcnt_names() {
    for side in SIDES {
        let other = SIDES[usize::from(side == Side::Arm9)];
        let mut hw = hardware();
        hw.exmemcnt = if side == Side::Arm7 { 1 << 11 } else { 0 };

        // AUXSPICNT keeps bits 0–1, 6 and 13–15, a byte at a time too.
        write(&mut hw, side, 2, 0x0400_01A0, 0xFFFF);
        assert_eq!(read(&mut hw, side, 2, 0x0400_01A0), 0xE043);
        write(&mut hw, side, 1, 0x0400_01A0, 0x01);
        assert_eq!(read(&mut hw, side, 2, 0x0400_01A0), 0xE001);
        write(&mut hw, side, 1, 0x0400_01A1, 0xA0);
        assert_eq!(read(&mut hw, side, 2, 0x0400_01A0), 0xA001);
        assert_eq!(raw(&hw, side, 0x1A0) & 0xFFFF, 0xA001);

        // AUXSPIDATA: a halfword or its low byte sends a byte to the save
        // chip, and AUXSPICNT is busy until the transfer's event.
        for bytes in [2, 1] {
            write(&mut hw, side, bytes, 0x0400_01A2, 0x05);
            assert_eq!(read(&mut hw, side, 2, 0x0400_01A0), 0xA081, "{bytes}: busy");
            hw.cart.finish_spi();
            hw.scheduler.cancel(|event| *event == Event::CartSpiDone);
            assert_eq!(read(&mut hw, side, 2, 0x0400_01A0), 0xA001);
        }
        // A word at AUXSPICNT is the control, then the data.
        write(&mut hw, side, 4, 0x0400_01A0, 0x0005_A041);
        assert_eq!(read(&mut hw, side, 2, 0x0400_01A0), 0xA0C1);
        hw.cart.finish_spi();
        hw.scheduler.cancel(|event| *event == Event::CartSpiDone);
        // Outside SPI mode a write to the data sends nothing.
        write(&mut hw, side, 2, 0x0400_01A0, 0x8000);
        write(&mut hw, side, 2, 0x0400_01A2, 0x05);
        assert_eq!(read(&mut hw, side, 2, 0x0400_01A0), 0x8000);

        // The command's eight bytes, at any width. They do not read back.
        write(&mut hw, side, 4, 0x0400_01A8, 0x4433_2211);
        write(&mut hw, side, 2, 0x0400_01AC, 0x6655);
        write(&mut hw, side, 1, 0x0400_01AE, 0x77);
        write(&mut hw, side, 1, 0x0400_01AF, 0x88);
        write(&mut hw, side, 1, 0x0400_01A9, 0xEE);
        let command = saved(&hw.cart);
        assert!(
            command
                .windows(8)
                .any(|bytes| bytes == [0x11, 0xEE, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]),
            "the command bytes as written"
        );
        assert_eq!(read_word(&mut hw, side, 0x0400_01A8), 0);
        assert_eq!(raw(&hw, side, 0x1A8), 0x4433_EE11);
        assert_eq!(raw(&hw, side, 0x1AC), 0x8877_6655);

        // The other CPU reads zeros and its writes reach nothing, at any
        // width; they are kept all the same.
        let before = saved(&hw.cart);
        for bytes in [1, 2, 4] {
            for address in [0x0400_01A0, 0x0400_01A4, 0x0400_01A8, 0x0400_01AC] {
                write(&mut hw, other, bytes, address, 0xFFFF_FFFF);
            }
        }
        assert!(saved(&hw.cart) == before, "the card is untouched");
        assert_eq!(read_word(&mut hw, other, 0x0400_01A0), 0);
        assert_eq!(read_word(&mut hw, other, 0x0400_01A4), 0);
        assert_eq!(read(&mut hw, other, 4, 0x0410_0010), 0);
        assert_eq!(raw(&hw, other, 0x1A0), 0xFFFF_FFFF);
        assert_eq!(raw(&hw, other, 0x1A4), 0xFFFF_FFFF);
    }
}

#[test]
fn registers_kept_by_their_device_take_every_width_and_read_back_the_same() {
    // DMA 2's source on either CPU, the ARM9's fill 1 and numerator.
    let registers = [
        (Side::Arm9, 0x0400_00C8),
        (Side::Arm7, 0x0400_00C8),
        (Side::Arm9, 0x0400_00E4),
        (Side::Arm9, 0x0400_0290),
    ];
    for (side, address) in registers {
        let offset = (address & 0xFFF) as usize;
        let mut hw = hardware();
        // A word goes to the device alone.
        write(&mut hw, side, 4, address, 0x0123_4567);
        assert_eq!(read_word(&mut hw, side, address), 0x0123_4567);
        assert_eq!(
            raw(&hw, side, offset),
            0,
            "{address:#X}: a word is not kept"
        );
        // A halfword is kept as written.
        write(&mut hw, side, 2, address + 2, 0x0FED);
        assert_eq!(read_word(&mut hw, side, address), 0x0FED_4567);
        assert_eq!(raw(&hw, side, offset), 0x0FED_0000);
        // A byte keeps its halfword as the device held it, with the byte.
        write(&mut hw, side, 1, address + 1, 0xAA);
        assert_eq!(read_word(&mut hw, side, address), 0x0FED_AA67);
        assert_eq!(raw(&hw, side, offset), 0x0FED_AA67);
        write(&mut hw, side, 2, address, 0x1111);
        assert_eq!(read_word(&mut hw, side, address), 0x0FED_1111);
    }
    // The ARM7 has no fill registers or maths unit: those offsets are no
    // registers there, and read back what was written.
    let mut hw = hardware();
    for address in [0x0400_00E4, 0x0400_0290] {
        hw.arm7_write_32(address, 0x89AB_CDEF);
        hw.arm7_write_8(address + 3, 0x01);
        assert_eq!(read_word(&mut hw, Side::Arm7, address), 0x01AB_CDEF);
        assert_eq!(
            raw(&hw, Side::Arm7, (address & 0xFFF) as usize),
            0x01AB_CDEF
        );
    }
    assert_eq!(hw.arm9_read_32(0x0400_00E4), 0);
}

#[test]
fn a_dma_control_starts_its_channel_at_whichever_width_sets_the_enable_bit() {
    for side in SIDES {
        for bytes in [4, 2, 1] {
            let mut hw = hardware();
            write(&mut hw, side, 4, 0x0400_00B0, 0x0200_0000);
            write(&mut hw, side, 4, 0x0400_00B4, 0x0200_0100);
            write(&mut hw, side, 2, 0x0400_00B8, 1);
            assert_eq!(hw.cpu(side).dma.next_pending(), None);
            // Bit 31, in the top byte.
            let address = 0x0400_00B8 + (4 - bytes);
            write(
                &mut hw,
                side,
                bytes,
                address,
                0x8000_0000 >> ((4 - bytes) * 8),
            );
            assert_eq!(hw.cpu(side).dma.next_pending(), Some(0), "{bytes} bytes");
        }
    }
}

#[test]
fn romctrl_takes_every_width_from_the_cards_owner() {
    let mut hw = hardware();
    // Bits 0–12 and 24–30 are settings; bit 31 would start a transfer.
    hw.arm9_write_32(0x0400_01A4, 0x0041_6657);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_01A4), 0x0041_6657);
    assert_eq!(raw(&hw, Side::Arm9, 0x1A4), 0, "a word is not kept");
    hw.arm9_write_16(0x0400_01A6, 0x2041);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_01A4), 0x2041_6657);
    assert_eq!(raw(&hw, Side::Arm9, 0x1A4), 0x2041_0000);
    hw.arm9_write_8(0x0400_01A4, 0x11);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_01A4), 0x2041_6611);
    assert_eq!(raw(&hw, Side::Arm9, 0x1A4), 0x2041_6611);
    // The top byte alone starts a transfer: busy, and its step scheduled.
    hw.arm9_write_8(0x0400_01A7, 0xA0);
    assert_ne!(hw.arm9_read_32(0x0400_01A4) & 0x8000_0000, 0);
    assert!(
        hw.scheduler
            .pending()
            .iter()
            .any(|due| due.event == Event::CartStep)
    );
}

#[test]
fn the_3d_engines_registers_take_the_bytes_written() {
    let mut hw = hardware();
    // DISP3DCNT: bits 0–11 and 14 are kept.
    hw.arm9_write_16(0x0400_0060, 0xFFFF);
    assert_eq!(hw.arm9_read_16(0x0400_0060), 0x4FFF);
    hw.arm9_write_8(0x0400_0060, 0x00);
    assert_eq!(hw.arm9_read_16(0x0400_0060), 0x4F00);
    hw.arm9_write_8(0x0400_0061, 0x00);
    assert_eq!(hw.arm9_read_16(0x0400_0060), 0x0000);
    hw.arm9_write_32(0x0400_0060, 0xFFFF_0012);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_0060), 0x0000_0012);
    // A halfword is kept, a byte and a word are not.
    assert_eq!(raw(&hw, Side::Arm9, 0x060), 0x0000_FFFF);

    // GXSTAT: bits 30–31, the FIFO interrupt's condition, are its top
    // byte; the other bytes leave them.
    hw.arm9_write_32(0x0400_0600, 0xC000_0000);
    assert_eq!(hw.arm9_read_32(0x0400_0600) >> 30, 3);
    hw.arm9_write_16(0x0400_0600, 0x0000);
    hw.arm9_write_8(0x0400_0602, 0x00);
    assert_eq!(hw.arm9_read_32(0x0400_0600) >> 30, 3);
    hw.arm9_write_8(0x0400_0603, 0x40);
    assert_eq!(hw.arm9_read_32(0x0400_0600) >> 30, 1);
    hw.arm9_write_16(0x0400_0602, 0x8000);
    assert_eq!(hw.arm9_read_32(0x0400_0600) >> 30, 2);
    assert_eq!(raw(&hw, Side::Arm9, 0x600), 0x8000_0000);

    // The rendering registers are bytes: a word, halfwords and bytes
    // leave the same toon table.
    let toon = |writes: &dyn Fn(&mut Hardware)| {
        let mut hw = hardware();
        writes(&mut hw);
        saved(&hw.gpu3d)
    };
    let word = toon(&|hw| hw.arm9_write_32(0x0400_0380, 0x7C1F_03E0));
    let halves = toon(&|hw| {
        hw.arm9_write_16(0x0400_0380, 0x03E0);
        hw.arm9_write_16(0x0400_0382, 0x7C1F);
    });
    let bytes = toon(&|hw| hw_write_8s(hw, Side::Arm9, 0x0400_0380, &[0xE0, 0x03, 0x1F, 0x7C]));
    assert!(word == halves && word == bytes);
    assert!(word != toon(&|_| ()), "and it is not the table at power-on");
    // The ARM7 has no 3D engine.
    let untouched = toon(&|hw| hw.arm7_write_32(0x0400_0380, 0x7C1F_03E0));
    assert!(untouched == toon(&|_| ()));
}

#[test]
fn a_2d_engines_registers_are_halfwords_that_take_bytes_and_words() {
    // Engine A at 0x04000000 and B at 0x04001000. What the registers hold
    // is compared as a save state holds it: most cannot be read back.
    for base in [0x0400_0000, 0x0400_1000] {
        let engine = |hw: &Hardware| {
            saved(if base == 0x0400_0000 {
                &hw.engine_a
            } else {
                &hw.engine_b
            })
        };
        // BG0's control and scroll, BG2's matrix and reference point, the
        // windows, mosaic, blending and brightness.
        let registers: [(u32, u16); 12] = [
            (0x08, 0x5A83),
            (0x10, 0x01A5),
            (0x12, 0x0177),
            (0x20, 0x0180),
            (0x22, 0xFF40),
            (0x28, 0x5678),
            (0x2A, 0x0123),
            (0x40, 0x10F0),
            (0x4C, 0x4321),
            (0x50, 0x3F41),
            (0x54, 0x0010),
            (0x6C, 0x8010),
        ];
        let mut halves = hardware();
        let mut bytes = hardware();
        for (offset, value) in registers {
            halves.arm9_write_16(base + offset, value);
            // Low byte, then high: after the first, the halfword holds
            // that byte and what it held before.
            let [low, high] = value.to_le_bytes();
            bytes.arm9_write_8(base + offset, low);
            bytes.arm9_write_8(base + offset + 1, high);
        }
        assert!(engine(&halves) == engine(&bytes), "bytes make the halfword");
        assert!(engine(&halves) != engine(&hardware()));
        // A word is its two halfwords.
        let mut words = hardware();
        let mut pairs = hardware();
        for (offset, value) in [
            (0x10, 0x0177_01A5u32),
            (0x28, 0x0123_5678),
            (0x40, 0x20E0_10F0),
        ] {
            words.arm9_write_32(base + offset, value);
            pairs.arm9_write_16(base + offset, value as u16);
            pairs.arm9_write_16(base + offset + 2, (value >> 16) as u16);
        }
        assert!(engine(&words) == engine(&pairs));

        // DISPCNT reads back; engine B lacks some of its bits.
        let mut hw = hardware();
        hw.arm9_write_32(base, 0xFFFF_FFFF);
        let all = if base == 0x0400_0000 {
            0xFFFF_FFFF
        } else {
            0xC0B1_FFF7
        };
        assert_eq!(read_word(&mut hw, Side::Arm9, base), all);
        hw.arm9_write_8(base + 1, 0x00);
        hw.arm9_write_8(base + 3, 0x00);
        assert_eq!(read_word(&mut hw, Side::Arm9, base), all & 0x00FF_00FF);
        hw.arm9_write_16(base + 2, 0x0001);
        assert_eq!(read_word(&mut hw, Side::Arm9, base), 0x0001_00FF & all);
        // Every write to these is kept as written.
        assert_eq!(raw(&hw, Side::Arm9, (base & 0x1FFF) as usize), 0x0001_00FF);
        // BG0CNT reads back whole; the scroll beside nothing does not.
        hw.arm9_write_8(base + 0x09, 0xAB);
        hw.arm9_write_8(base + 0x08, 0xCD);
        assert_eq!(hw.arm9_read_16(base + 0x08), 0xABCD);
        hw.arm9_write_16(base + 0x10, 0x01FF);
        assert_eq!(hw.arm9_read_16(base + 0x10), 0);
    }
    // The ARM7 has neither engine.
    let mut hw = hardware();
    let before = (saved(&hw.engine_a), saved(&hw.engine_b));
    hw.arm7_write_32(0x0400_0000, 0xFFFF_FFFF);
    hw.arm7_write_16(0x0400_1008, 0xFFFF);
    assert!(before == (saved(&hw.engine_a), saved(&hw.engine_b)));
}

#[test]
fn exmemcnt_is_the_arm9s_and_the_arm7_keeps_seven_bits_of_its_own() {
    let mut hw = hardware();
    hw.arm9_write_16(0x0400_0204, 0xE8FF);
    assert_eq!(hw.exmemcnt, 0xE8FF);
    hw.arm9_write_8(0x0400_0205, 0x08);
    assert_eq!(hw.exmemcnt, 0x08FF);
    hw.arm9_write_8(0x0400_0204, 0x80);
    assert_eq!(hw.exmemcnt, 0x0880);
    hw.arm9_write_32(0x0400_0204, 0xFFFF_4855);
    assert_eq!(hw.exmemcnt, 0x4855);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_0204), 0xFFFF_4855);
    // The ARM7 reads the ARM9's bits 7–15 above its own bits 0–6, and
    // writes only its own.
    assert_eq!(hw.arm7_read_16(0x0400_0204), 0x4800);
    hw.arm7_write_16(0x0400_0204, 0xFFFF);
    assert_eq!(hw.exmemcnt, 0x4855);
    assert_eq!(hw.arm7_read_16(0x0400_0204), 0x487F);
    hw.arm7_write_8(0x0400_0204, 0x2A);
    assert_eq!(hw.arm7_read_16(0x0400_0204), 0x482A);
    assert_eq!(hw.gba_slot_timing(Side::Arm7), 0x2A);
    assert_eq!(hw.gba_slot_timing(Side::Arm9), 0x55);
}

#[test]
fn the_power_controls_keep_their_bits_at_every_width() {
    // POWCNT1 is the ARM9's: bits 0–3, 9 and 15.
    let mut hw = hardware();
    hw.arm9_write_16(0x0400_0304, 0xFFFF);
    assert_eq!(hw.powcnt1, 0x820F);
    hw.arm9_write_8(0x0400_0305, 0x02);
    assert_eq!(hw.powcnt1, 0x020F);
    hw.arm9_write_8(0x0400_0304, 0x03);
    assert_eq!(hw.powcnt1, 0x0203);
    hw.arm9_write_32(0x0400_0304, 0xFFFF_8001);
    assert_eq!(hw.powcnt1, 0x8001);
    assert_eq!(read_word(&mut hw, Side::Arm9, 0x0400_0304), 0xFFFF_8001);
    assert_eq!(hw.powcnt2, 0, "the ARM7's is its own");

    // POWCNT2 is the ARM7's: bit 0 the speakers, bit 1 the wifi.
    let mut hw = hardware();
    hw.arm7_write_16(0x0400_0304, 0xFFFF);
    assert_eq!(hw.powcnt2, 3);
    hw.arm7_write_8(0x0400_0305, 0xFF);
    assert_eq!(hw.powcnt2, 3);
    hw.arm7_write_8(0x0400_0304, 0x01);
    assert_eq!(hw.powcnt2, 1);
    hw.arm7_write_32(0x0400_0304, 0xFFFF_0002);
    assert_eq!(hw.powcnt2, 2);
    assert_eq!(read_word(&mut hw, Side::Arm7, 0x0400_0304), 0xFFFF_0002);
    assert_eq!(hw.powcnt1, 0);
}

#[test]
fn the_arm7s_serial_buses_and_clock_take_bytes_and_halfwords() {
    // SPICNT keeps its settings; the data's low byte sends.
    let mut hw = hardware();
    hw.arm7_write_16(0x0400_01C0, 0xFFFF);
    let all = hw.arm7_read_16(0x0400_01C0);
    assert_eq!(all & 0x8080, 0x8000, "enabled, not busy");
    hw.arm7_write_8(0x0400_01C0, 0x00);
    assert_eq!(hw.arm7_read_16(0x0400_01C0), all & 0xFF00);
    hw.arm7_write_8(0x0400_01C1, 0x80);
    assert_eq!(hw.arm7_read_16(0x0400_01C0), 0x8000);
    for bytes in [2, 1] {
        write(&mut hw, Side::Arm7, bytes, 0x0400_01C2, 0x00);
        assert_eq!(hw.arm7_read_16(0x0400_01C0), 0x8080, "{bytes}: busy");
        assert!(!hw.spi.finish(), "no interrupt asked for");
        hw.scheduler.cancel(|event| *event == Event::SpiDone);
    }
    // A word at SPICNT is the control, then the data.
    hw.arm7_write_32(0x0400_01C0, 0x0000_8001);
    assert_eq!(hw.arm7_read_16(0x0400_01C0), 0x8081);
    assert_eq!(raw(&hw, Side::Arm7, 0x1C0), 0x0000_8001);

    // The clock's pins are the low byte of 0x04000138; all of them
    // outputs, they read back.
    let mut hw = hardware();
    hw.arm7_write_16(0x0400_0138, 0x0077);
    assert_eq!(hw.arm7_read_16(0x0400_0138), 0x0077);
    hw.arm7_write_8(0x0400_0138, 0x74);
    assert_eq!(hw.arm7_read_16(0x0400_0138), 0x0074);
    hw.arm7_write_8(0x0400_0139, 0x12);
    assert_eq!(hw.arm7_read_16(0x0400_0138), 0x1274);
    assert_eq!(raw(&hw, Side::Arm7, 0x138), 0x1274);
}

#[test]
fn sound_registers_take_the_bytes_written_and_no_others() {
    let mut hw = hardware();
    // SOUND0CNT keeps bits 0–6, 8–9, 15 and 16–31 but 23.
    hw.arm7_write_32(0x0400_0400, 0x7FFF_FFFF);
    assert_eq!(read_word(&mut hw, Side::Arm7, 0x0400_0400), 0x7F7F_837F);
    hw.arm7_write_8(0x0400_0402, 0x00);
    assert_eq!(hw.arm7_read_32(0x0400_0400), 0x7F00_837F);
    hw.arm7_write_16(0x0400_0400, 0x0040);
    assert_eq!(hw.arm7_read_32(0x0400_0400), 0x7F00_0040);
    hw.arm7_write_8(0x0400_0401, 0x80);
    assert_eq!(hw.arm7_read_32(0x0400_0400), 0x7F00_8040);
    // A word and a halfword are kept, a byte is not.
    assert_eq!(raw(&hw, Side::Arm7, 0x400), 0x7FFF_0040);

    // SOUNDCNT and SOUNDBIAS.
    hw.arm7_write_16(0x0400_0500, 0xFFFF);
    assert_eq!(hw.arm7_read_16(0x0400_0500), 0xBF7F);
    hw.arm7_write_8(0x0400_0501, 0x80);
    assert_eq!(hw.arm7_read_16(0x0400_0500), 0x807F);
    hw.arm7_write_32(0x0400_0504, 0xFFFF_FFFF);
    assert_eq!(hw.arm7_read_32(0x0400_0504), 0x3FF);
    hw.arm7_write_8(0x0400_0504, 0x00);
    assert_eq!(hw.arm7_read_32(0x0400_0504), 0x300);

    // The timer and loop start share a word and do not read back: a
    // word, halfwords and bytes leave the same channel.
    let channel = |writes: &dyn Fn(&mut Hardware)| {
        let mut hw = hardware();
        writes(&mut hw);
        saved(&hw.sound)
    };
    let word = channel(&|hw| hw.arm7_write_32(0x0400_0418, 0x1234_FF80));
    let halves = channel(&|hw| {
        hw.arm7_write_16(0x0400_0418, 0xFF80);
        hw.arm7_write_16(0x0400_041A, 0x1234);
    });
    let bytes = channel(&|hw| hw_write_8s(hw, Side::Arm7, 0x0400_0418, &[0x80, 0xFF, 0x34, 0x12]));
    assert!(word == halves && word == bytes);
    assert!(word != channel(&|_| ()));
    // The ARM9 has no sound registers.
    let untouched = channel(&|hw| hw.arm9_write_32(0x0400_0418, 0x1234_FF80));
    assert!(untouched == channel(&|_| ()));
}

#[test]
fn an_offset_with_no_register_reads_back_what_was_written_at_any_width() {
    // DISPCAPCNT is kept this way, and so are the offsets nothing uses.
    let offsets = [
        (Side::Arm9, 0x0400_0064),
        (Side::Arm9, 0x0400_0068),
        (Side::Arm9, 0x0400_0308),
        (Side::Arm9, 0x0400_1FFC),
        (Side::Arm7, 0x0400_0600),
        (Side::Arm7, 0x0400_0308),
        (Side::Arm7, 0x0400_1000),
    ];
    for (side, address) in offsets {
        let offset = (address & 0x1FFF) as usize;
        let mut hw = hardware();
        write(&mut hw, side, 4, address, 0x1122_3344);
        assert_eq!(read_word(&mut hw, side, address), 0x1122_3344);
        write(&mut hw, side, 2, address + 2, 0xAABB);
        assert_eq!(read_word(&mut hw, side, address), 0xAABB_3344);
        write(&mut hw, side, 1, address + 1, 0xCC);
        write(&mut hw, side, 1, address + 3, 0xDD);
        assert_eq!(read_word(&mut hw, side, address), 0xDDBB_CC44);
        assert_eq!(raw(&hw, side, offset), 0xDDBB_CC44);
        // The other CPU's page is its own.
        let other = SIDES[usize::from(side == Side::Arm9)];
        assert_eq!(raw(&hw, other, offset), 0);
    }
    // RCNT is one of them; EXTKEYIN beside it cannot be written.
    let mut hw = hardware();
    hw.arm7_write_32(0x0400_0134, 0xFFFF_8000);
    hw.arm7_write_8(0x0400_0136, 0x00);
    assert_eq!(hw.ext_keys, 0x007F);
    assert_eq!(read_word(&mut hw, Side::Arm7, 0x0400_0134), 0x007F_8000);
    assert_eq!(raw(&hw, Side::Arm7, 0x134), 0xFF00_8000);
    // Nothing above the page is kept or read back.
    let mut hw = hardware();
    for side in SIDES {
        write(&mut hw, side, 4, 0x0400_2000, 0xFFFF_FFFF);
        write(&mut hw, side, 2, 0x0401_0064, 0xFFFF);
        write(&mut hw, side, 1, 0x0410_0064, 0xFF);
        assert_eq!(read(&mut hw, side, 4, 0x0400_2000), 0);
        assert_eq!(raw(&hw, side, 0x000), 0);
        assert_eq!(raw(&hw, side, 0x064), 0);
    }
}

// ----- What a byte written beside a register's other byte leaves alone ----
//
// A byte write used to be merged with the raw copy of what the CPU last
// wrote to that halfword, and the halfword written again. Where the raw
// copy was not what the register held, that put something old back. Each
// test here says what such a write did before every write took one path.

/// A bit that only acts when a one is written is not in the register, but
/// it was in the raw copy: a byte written beside it acted again.
#[test]
fn a_byte_written_beside_a_bit_that_only_acts_does_not_act_again() {
    for side in SIDES {
        let other = SIDES[usize::from(side == Side::Arm9)];
        // IPCSYNC's bit 13 sends the other CPU an interrupt. A byte then
        // written below it sent another.
        let mut hw = hardware();
        write(&mut hw, other, 2, 0x0400_0180, 0x4000);
        write(&mut hw, side, 2, 0x0400_0180, 0x2500);
        assert_eq!(hw.cpu(other).interrupts.flags, irq::IPC_SYNC);
        hw.cpu_mut(other).interrupts.flags = 0;
        write(&mut hw, side, 1, 0x0400_0180, 0x00);
        assert_eq!(hw.cpu(other).interrupts.flags, 0);
        assert_eq!(
            read(&mut hw, side, 2, 0x0400_0180),
            0x0500,
            "the data stays"
        );

        // IPCFIFOCNT's bit 3 flushes the words sent. A byte then written
        // above it flushed the next word sent too.
        let mut hw = hardware();
        write(&mut hw, side, 2, 0x0400_0184, 0x8008);
        write(&mut hw, side, 4, 0x0400_0188, 0x1234_5678);
        write(&mut hw, side, 1, 0x0400_0185, 0x80);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184) & 1, 0, "the word waits");
        assert_eq!(read(&mut hw, other, 4, 0x0410_0000), 0x1234_5678);

        // Its bit 14 acknowledges an error. A byte then written below it
        // acknowledged the next error too.
        let mut hw = hardware();
        write(&mut hw, side, 2, 0x0400_0184, 0x8000);
        for word in 0..17 {
            write(&mut hw, side, 4, 0x0400_0188, word);
        }
        write(&mut hw, side, 2, 0x0400_0184, 0xC000);
        assert_eq!(read(&mut hw, side, 2, 0x0400_0184) & 0x4000, 0);
        write(&mut hw, side, 4, 0x0400_0188, 17);
        assert_ne!(read(&mut hw, side, 2, 0x0400_0184) & 0x4000, 0, "an error");
        write(&mut hw, side, 1, 0x0400_0184, 0x00);
        assert_ne!(
            read(&mut hw, side, 2, 0x0400_0184) & 0x4000,
            0,
            "still there"
        );
    }
}

/// The byte a serial bus sent is not kept by the bus, but it was in the
/// raw copy: a byte written above the data register sent it again.
#[test]
fn only_the_data_byte_of_a_serial_bus_sends() {
    let mut hw = hardware();
    hw.arm9_write_16(0x0400_01A0, 0xA000);
    hw.arm9_write_16(0x0400_01A2, 0x0005);
    hw.cart.finish_spi();
    hw.scheduler.cancel(|event| *event == Event::CartSpiDone);
    hw.arm9_write_8(0x0400_01A3, 0xFF);
    assert_eq!(hw.arm9_read_16(0x0400_01A0), 0xA000, "nothing was sent");
    assert!(
        !hw.scheduler
            .pending()
            .iter()
            .any(|due| due.event == Event::CartSpiDone)
    );

    hw.arm7_write_16(0x0400_01C0, 0x8000);
    hw.arm7_write_16(0x0400_01C2, 0x0000);
    hw.spi.finish();
    hw.scheduler.cancel(|event| *event == Event::SpiDone);
    hw.arm7_write_8(0x0400_01C3, 0xFF);
    assert_eq!(hw.arm7_read_16(0x0400_01C0), 0x8000, "nothing was sent");
    assert!(
        !hw.scheduler
            .pending()
            .iter()
            .any(|due| due.event == Event::SpiDone)
    );
}

/// A byte written to the ARM9's POSTFLG is not kept in the raw copy, so a
/// byte then written above it set POSTFLG back to what the last halfword
/// written there had held.
#[test]
fn a_byte_written_above_the_arm9s_postflg_leaves_it() {
    let mut hw = hardware();
    hw.arm9_write_8(0x0400_0300, 1);
    hw.arm9_write_8(0x0400_0301, 0xFF);
    assert_eq!(hw.arm9.postflg, 1);
    hw.arm9_write_16(0x0400_0300, 0x0002);
    hw.arm9_write_8(0x0400_0300, 1);
    hw.arm9_write_8(0x0400_0301, 0x00);
    assert_eq!(hw.arm9.postflg, 1);
}

/// A register set without a write of the CPU's (at power-on, or by the
/// boot that stands in for the firmware) was never in the raw copy: a
/// byte written to it was merged with zero.
#[test]
fn a_byte_written_to_a_register_set_without_a_write_keeps_its_other_byte() {
    // The affine matrices are the identity at power-on: PA is 1.0, 0x0100.
    for base in [0x0400_0000, 0x0400_1000] {
        let engine = |hw: &Hardware| {
            saved(if base == 0x0400_0000 {
                &hw.engine_a
            } else {
                &hw.engine_b
            })
        };
        let mut byte = hardware();
        byte.arm9_write_8(base + 0x20, 0x80);
        let mut halfword = hardware();
        halfword.arm9_write_16(base + 0x20, 0x0180);
        assert!(engine(&byte) == engine(&halfword), "PA is 0x0180");
    }
    // Direct boot leaves POWCNT1 with both screens and engines on, and
    // POWCNT2 with the speakers on.
    let mut hw = hardware();
    hw.powcnt1 = 0x820F;
    hw.powcnt2 = 1;
    hw.arm9_write_8(0x0400_0305, 0x02);
    assert_eq!(hw.powcnt1, 0x020F, "the screens stay on");
    hw.arm7_write_8(0x0400_0305, 0x00);
    assert_eq!(hw.powcnt2, 1, "the speakers stay on");
}

/// Each CPU has a raw copy of its own, and a write to the game card from
/// the CPU that does not own it is kept there though it reaches nothing.
/// Once that CPU owned the card, a byte it wrote was merged with what it
/// had written before, not with what the card held.
#[test]
fn a_byte_written_to_the_game_card_by_its_new_owner_is_merged_with_the_cards_own() {
    let mut hw = hardware();
    // The ARM7's, while the card is the ARM9's.
    hw.arm7_write_16(0x0400_01A0, 0xFFFF);
    hw.arm7_write_32(0x0400_01A8, 0xFFFF_FFFF);
    // The ARM9's, which the card holds.
    hw.arm9_write_16(0x0400_01A0, 0xA000);
    hw.arm9_write_32(0x0400_01A8, 0x4433_2211);
    hw.arm9_write_32(0x0400_01AC, 0x8877_6655);
    // The card goes to the ARM7, which writes a byte of each.
    hw.arm9_write_16(0x0400_0204, 1 << 11);
    hw.arm7_write_8(0x0400_01A0, 0x01);
    assert_eq!(hw.arm7_read_16(0x0400_01A0), 0xA001);
    hw.arm7_write_8(0x0400_01A8, 0x99);
    let card = saved(&hw.cart);
    assert!(
        card.windows(8)
            .any(|bytes| bytes == [0x99, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]),
        "the command's other bytes are the card's"
    );
}
