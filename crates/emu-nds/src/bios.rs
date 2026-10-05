//! A high-level emulation of the two DS BIOSes (GBATEK, "BIOS Functions",
//! DS columns).
//!
//! No Nintendo code is used. Each CPU gets a small vector table written
//! here. The ARM7's IRQ vector calls the handler at 0x0380FFFC, and the
//! ARM9's calls the one at the top of DTCM, which it finds through CP15.
//! System calls never run BIOS code: the machine catches the CPU entering
//! the SWI vector and runs [`run`] instead, then returns from the
//! exception.
//!
//! A call is a pure function of its register arguments and memory: it
//! touches memory through the [`Bus`] it is given (which, for the ARM9,
//! also reaches its TCM) and returns the register changes for the machine
//! to apply.

use emu_arm::{Access, Bus};

use crate::ipc::Side;

/// The ARM9 BIOS (4 KiB at 0xFFFF0000).
pub(crate) fn arm9_image() -> Box<[u8]> {
    let mut image = vec![0u8; 0x1000];
    let mut put = |address: usize, instruction: u32| {
        image[address..address + 4].copy_from_slice(&instruction.to_le_bytes());
    };
    put(0x00, 0xEAFF_FFFE); // reset: B .
    put(0x04, 0xE1B0_F00E); // undefined: MOVS PC, LR
    put(0x08, 0xE1B0_F00E); // SWI: MOVS PC, LR (calls are intercepted first)
    put(0x0C, 0xE25E_F004); // prefetch abort: SUBS PC, LR, #4
    put(0x10, 0xE25E_F008); // data abort: SUBS PC, LR, #8
    put(0x18, 0xEA00_0038); // IRQ: B 0x100
    put(0x1C, 0xE25E_F004); // FIQ: SUBS PC, LR, #4
    put(0x100, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    put(0x104, 0xEE19_0F11); // MRC P15, 0, R0, C9, C1, 0  (DTCM region)
    put(0x108, 0xE1A0_0620); // MOV R0, R0, LSR #12
    put(0x10C, 0xE1A0_0600); // MOV R0, R0, LSL #12  (DTCM base)
    put(0x110, 0xE280_0901); // ADD R0, R0, #0x4000
    put(0x114, 0xE28F_E000); // ADD LR, PC, #0
    put(0x118, 0xE510_F004); // LDR PC, [R0, #-4]  (handler at DTCM + 0x3FFC)
    put(0x11C, 0xE8BD_500F); // LDMFD SP!, {R0-R3, R12, LR}
    put(0x120, 0xE25E_F004); // SUBS PC, LR, #4
    image.into_boxed_slice()
}

/// The ARM7 BIOS (16 KiB at 0).
pub(crate) fn arm7_image() -> Box<[u8]> {
    let mut image = vec![0u8; 0x4000];
    let mut put = |address: usize, instruction: u32| {
        image[address..address + 4].copy_from_slice(&instruction.to_le_bytes());
    };
    put(0x00, 0xEAFF_FFFE); // reset: B .
    put(0x04, 0xE1B0_F00E); // undefined: MOVS PC, LR
    put(0x08, 0xE1B0_F00E); // SWI: MOVS PC, LR (calls are intercepted first)
    put(0x0C, 0xE25E_F004); // prefetch abort: SUBS PC, LR, #4
    put(0x10, 0xE25E_F008); // data abort: SUBS PC, LR, #8
    put(0x18, 0xEA00_0038); // IRQ: B 0x100
    put(0x1C, 0xE25E_F004); // FIQ: SUBS PC, LR, #4
    put(0x100, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    put(0x104, 0xE3A0_0301); // MOV R0, #0x04000000
    put(0x108, 0xE28F_E000); // ADD LR, PC, #0
    put(0x10C, 0xE510_F004); // LDR PC, [R0, #-4]  (0x03FFFFFC mirrors 0x0380FFFC)
    put(0x110, 0xE8BD_500F); // LDMFD SP!, {R0-R3, R12, LR}
    put(0x114, 0xE25E_F004); // SUBS PC, LR, #4
    image.into_boxed_slice()
}

/// R15 once a CPU has entered its SWI vector (the vector plus 8).
pub(crate) const ARM9_SWI_ENTERED_PC: u32 = 0xFFFF_0008 + 8;
pub(crate) const ARM7_SWI_ENTERED_PC: u32 = 0x08 + 8;

/// A system call as the CPU made it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Call {
    pub(crate) side: Side,
    /// R0–R3.
    pub(crate) args: [u32; 4],
    /// Where the BIOS keeps its interrupt flags for IntrWait: DTCM + 0x3FF8
    /// on the ARM9, 0x0380FFF8 on the ARM7.
    pub(crate) intr_flags: u32,
}

/// What a call does to the CPU, for the machine to apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Effects {
    /// Registers to set, by index.
    pub(crate) registers: Vec<(usize, u32)>,
    /// Halt until an interrupt.
    pub(crate) halt: bool,
    /// Run the SWI again after the interrupt that ends the halt (IntrWait).
    pub(crate) repeat: bool,
    /// Extra cycles the call takes (WaitByLoop), in the CPU's clock.
    pub(crate) stall: u64,
    /// SoftReset: restart the game from its entry point.
    pub(crate) soft_reset: bool,
}

/// The function number from the SWI instruction that trapped: bits 16–23
/// of an ARM SWI, the low byte of a Thumb one.
pub(crate) fn number<B: Bus>(bus: &mut B, return_address: u32, thumb: bool) -> u8 {
    if thumb {
        bus.read_16(return_address.wrapping_sub(2), Access::NONSEQUENTIAL) as u8
    } else {
        (bus.read_32(return_address.wrapping_sub(4), Access::NONSEQUENTIAL) >> 16) as u8
    }
}

/// Runs system call `number`.
pub(crate) fn run<B: Bus>(number: u8, call: &Call, bus: &mut B) -> Effects {
    let [r0, r1, r2, _] = call.args;
    let arm7 = call.side == Side::Arm7;
    let set = |registers: &[(usize, u32)]| Effects {
        registers: registers.to_vec(),
        ..Effects::default()
    };
    match number {
        0x00 => Effects {
            soft_reset: true,
            ..Effects::default()
        },
        0x03 => Effects {
            // Each iteration of the BIOS loop takes four cycles.
            stall: u64::from(r0) * 4,
            ..Effects::default()
        },
        0x04 => intr_wait(bus, call.intr_flags, r0 != 0, r1),
        0x05 => intr_wait(bus, call.intr_flags, true, 1),
        0x06 => Effects {
            halt: true,
            ..Effects::default()
        },
        0x07 if arm7 => Effects {
            halt: true,
            ..Effects::default()
        },
        0x08 if arm7 => {
            bus.write_16(0x0400_0504, if r0 != 0 { 0x200 } else { 0 }, Access::NONSEQUENTIAL);
            Effects::default()
        }
        0x09 => set(&divide(r0 as i32, r1 as i32)),
        0x0B => {
            cpu_set(bus, r0, r1, r2);
            Effects::default()
        }
        0x0C => {
            cpu_fast_set(bus, r0, r1, r2);
            Effects::default()
        }
        0x0D => set(&[(0, (r0).isqrt())]),
        0x0E => set(&crc16_call(bus, r0 as u16, r1, r2)),
        0x0F => set(&[(0, 0)]),
        0x10 => {
            bit_unpack(bus, r0, r1, r2);
            Effects::default()
        }
        0x11 => {
            lz77(bus, r0, r1, false);
            Effects::default()
        }
        0x14 => {
            run_length(bus, r0, r1, false);
            Effects::default()
        }
        0x16 if !arm7 => {
            diff_unfilter(bus, r0, r1, false);
            Effects::default()
        }
        0x18 if !arm7 => {
            diff_unfilter(bus, r0, r1, true);
            Effects::default()
        }
        0x1A if arm7 => set(&[(0, sine_table(r0))]),
        0x1B if arm7 => set(&[(0, pitch_table(r0))]),
        0x1C if arm7 => set(&[(0, volume_table(r0))]),
        // ARM9 CustomPost: write POSTFLG.
        0x1F if !arm7 => {
            bus.write_32(0x0400_0300, r0, Access::NONSEQUENTIAL);
            Effects::default()
        }
        // ARM7 CustomHalt: write HALTCNT.
        0x1F => {
            bus.write_8(0x0400_0301, r2 as u8, Access::NONSEQUENTIAL);
            Effects::default()
        }
        // The callback decompressors (12h, 13h, 15h) need to run game code
        // and are not emulated yet, nor are the rest.
        _ => Effects::default(),
    }
}

/// IntrWait: returns once one of `mask`'s interrupts has been flagged by
/// the game's handler at `flags_address`. While waiting, the CPU halts and
/// the call runs again after each interrupt; R0 is zeroed so the repeat
/// does not discard flags the handler just set.
fn intr_wait<B: Bus>(bus: &mut B, flags_address: u32, discard: bool, mask: u32) -> Effects {
    let mut current = bus.read_32(flags_address, Access::NONSEQUENTIAL);
    if discard {
        current &= !mask;
        bus.write_32(flags_address, current, Access::NONSEQUENTIAL);
    }
    bus.write_32(0x0400_0208, 1, Access::NONSEQUENTIAL);
    if current & mask != 0 {
        bus.write_32(flags_address, current & !mask, Access::NONSEQUENTIAL);
        return Effects::default();
    }
    Effects {
        registers: vec![(0, 0)],
        halt: true,
        repeat: true,
        ..Effects::default()
    }
}

/// Div: R0 / R1 into R0, the remainder into R1 and |quotient| into R3.
fn divide(numerator: i32, denominator: i32) -> [(usize, u32); 3] {
    if denominator == 0 {
        // The real BIOS loops forever; return something harmless instead.
        let quotient: i32 = if numerator < 0 { -1 } else { 1 };
        return [(0, quotient as u32), (1, numerator as u32), (3, 1)];
    }
    let quotient = numerator.wrapping_div(denominator);
    [
        (0, quotient as u32),
        (1, numerator.wrapping_rem(denominator) as u32),
        (3, quotient.unsigned_abs()),
    ]
}

/// The CRC-16 (polynomial 0xA001, reflected) of `bytes`, starting from
/// `crc`, as GetCRC16 and the firmware compute it.
pub(crate) fn crc16(mut crc: u16, bytes: impl IntoIterator<Item = u8>) -> u16 {
    for byte in bytes {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// GetCRC16 over `length` bytes (whole halfwords) at `address`. R3 gets the
/// last halfword read.
fn crc16_call<B: Bus>(bus: &mut B, initial: u16, address: u32, length: u32) -> [(usize, u32); 2] {
    let mut crc = initial;
    let mut last = 0;
    for offset in (0..length & !1).step_by(2) {
        last = bus.read_16(address.wrapping_add(offset), Access::SEQUENTIAL);
        crc = crc16(crc, last.to_le_bytes());
    }
    [(0, u32::from(crc)), (3, u32::from(last))]
}

/// CpuSet: copies or fills `control & 0x1FFFFF` halfwords or words.
fn cpu_set<B: Bus>(bus: &mut B, source: u32, destination: u32, control: u32) {
    let count = control & 0x1F_FFFF;
    let fill = control & (1 << 24) != 0;
    let words = control & (1 << 26) != 0;
    let step = if words { 4 } else { 2 };
    let (mut source, mut destination) = if words {
        (source & !3, destination & !3)
    } else {
        (source & !1, destination & !1)
    };
    let fill_value = if words {
        bus.read_32(source, Access::NONSEQUENTIAL)
    } else {
        u32::from(bus.read_16(source, Access::NONSEQUENTIAL))
    };
    for _ in 0..count {
        let value = if fill {
            fill_value
        } else if words {
            bus.read_32(source, Access::SEQUENTIAL)
        } else {
            u32::from(bus.read_16(source, Access::SEQUENTIAL))
        };
        if words {
            bus.write_32(destination, value, Access::SEQUENTIAL);
        } else {
            bus.write_16(destination, value as u16, Access::SEQUENTIAL);
        }
        source = source.wrapping_add(step);
        destination = destination.wrapping_add(step);
    }
}

/// CpuFastSet: word copies or fills in blocks of eight words.
fn cpu_fast_set<B: Bus>(bus: &mut B, source: u32, destination: u32, control: u32) {
    let count = (control & 0x1F_FFFF).div_ceil(8) * 8;
    let fill = control & (1 << 24) != 0;
    let (mut source, mut destination) = (source & !3, destination & !3);
    let fill_value = bus.read_32(source, Access::NONSEQUENTIAL);
    for _ in 0..count {
        let value = if fill {
            fill_value
        } else {
            let value = bus.read_32(source, Access::SEQUENTIAL);
            source = source.wrapping_add(4);
            value
        };
        bus.write_32(destination, value, Access::SEQUENTIAL);
        destination = destination.wrapping_add(4);
    }
}

/// BitUnPack: widens packed pixels, adding an offset to non-zero (or all)
/// values.
fn bit_unpack<B: Bus>(bus: &mut B, mut source: u32, mut destination: u32, info: u32) {
    let length = u32::from(bus.read_16(info, Access::NONSEQUENTIAL));
    let source_width = u32::from(bus.read_8(info + 2, Access::NONSEQUENTIAL));
    let destination_width = u32::from(bus.read_8(info + 3, Access::NONSEQUENTIAL));
    let offset_field = bus.read_32(info + 4, Access::NONSEQUENTIAL);
    let offset = offset_field & 0x7FFF_FFFF;
    let offset_zero = offset_field >> 31 != 0;
    if !matches!(source_width, 1 | 2 | 4 | 8)
        || !matches!(destination_width, 1 | 2 | 4 | 8 | 16 | 32)
    {
        return;
    }
    let mut out = 0u32;
    let mut out_bits = 0;
    for _ in 0..length {
        let byte = u32::from(bus.read_8(source, Access::NONSEQUENTIAL));
        source += 1;
        for shift in (0..8).step_by(source_width as usize) {
            let mut value = (byte >> shift) & ((1 << source_width) - 1);
            if value != 0 || offset_zero {
                value = value.wrapping_add(offset);
            }
            out |= value.checked_shl(out_bits).unwrap_or(0);
            out_bits += destination_width;
            if out_bits >= 32 {
                bus.write_32(destination, out, Access::NONSEQUENTIAL);
                destination += 4;
                out = 0;
                out_bits = 0;
            }
        }
    }
}

/// LZ77UnCompReadNormalWrite8bit: GBATEK's LZ77 format with a 4-byte
/// header, written a byte at a time.
fn lz77<B: Bus>(bus: &mut B, mut source: u32, mut destination: u32, _halfwords: bool) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source += 4;
    while remaining > 0 {
        let flags = bus.read_8(source, Access::NONSEQUENTIAL);
        source += 1;
        for bit in (0..8).rev() {
            if remaining == 0 {
                break;
            }
            if flags & (1 << bit) == 0 {
                let byte = bus.read_8(source, Access::NONSEQUENTIAL);
                source += 1;
                bus.write_8(destination, byte, Access::NONSEQUENTIAL);
                destination += 1;
                remaining -= 1;
            } else {
                let high = u32::from(bus.read_8(source, Access::NONSEQUENTIAL));
                let low = u32::from(bus.read_8(source + 1, Access::NONSEQUENTIAL));
                source += 2;
                let length = (high >> 4) + 3;
                let distance = ((high & 0xF) << 8 | low) + 1;
                for _ in 0..length.min(remaining) {
                    let byte = bus.read_8(destination.wrapping_sub(distance), Access::NONSEQUENTIAL);
                    bus.write_8(destination, byte, Access::NONSEQUENTIAL);
                    destination += 1;
                    remaining -= 1;
                }
            }
        }
    }
}

/// RLUnCompReadNormalWrite8bit: runs of repeated bytes and literal
/// stretches.
fn run_length<B: Bus>(bus: &mut B, mut source: u32, mut destination: u32, _halfwords: bool) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source += 4;
    while remaining > 0 {
        let flag = bus.read_8(source, Access::NONSEQUENTIAL);
        source += 1;
        let literal = flag & 0x80 == 0;
        let length = if literal {
            u32::from(flag & 0x7F) + 1
        } else {
            u32::from(flag & 0x7F) + 3
        };
        let repeated = if literal {
            None
        } else {
            let byte = bus.read_8(source, Access::NONSEQUENTIAL);
            source += 1;
            Some(byte)
        };
        for _ in 0..length.min(remaining) {
            let byte = repeated.unwrap_or_else(|| {
                let byte = bus.read_8(source, Access::NONSEQUENTIAL);
                source += 1;
                byte
            });
            bus.write_8(destination, byte, Access::NONSEQUENTIAL);
            destination += 1;
            remaining -= 1;
        }
    }
}

/// Diff8bitUnFilterWrite8bit and Diff16bitUnFilter: running sums.
fn diff_unfilter<B: Bus>(bus: &mut B, mut source: u32, mut destination: u32, sixteen: bool) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source += 4;
    if sixteen {
        let mut sum = 0u16;
        while remaining >= 2 {
            sum = sum.wrapping_add(bus.read_16(source, Access::NONSEQUENTIAL));
            bus.write_16(destination, sum, Access::NONSEQUENTIAL);
            source += 2;
            destination += 2;
            remaining -= 2;
        }
        return;
    }
    let mut sum = 0u8;
    while remaining > 0 {
        sum = sum.wrapping_add(bus.read_8(source, Access::NONSEQUENTIAL));
        bus.write_8(destination, sum, Access::NONSEQUENTIAL);
        source += 1;
        destination += 1;
        remaining -= 1;
    }
}

/// GetSineTable: sin(index × 90° / 64) in 1.15 fixed point, index 0–63.
fn sine_table(index: u32) -> u32 {
    let angle = f64::from(index & 0x3F) * std::f64::consts::FRAC_PI_2 / 64.0;
    (angle.sin() * 32768.0).round() as u32
}

/// GetPitchTable: (2^(index / 768) − 1) × 0x10000, index 0–767, which the
/// sound driver uses to turn semitone fractions into timer values.
fn pitch_table(index: u32) -> u32 {
    let fraction = f64::from(index.min(767)) / 768.0;
    ((fraction.exp2() - 1.0) * 65536.0) as u32
}

/// GetVolumeTable: the 7-bit volume for attenuation `index`, 0–723, in
/// tenths of a decibel above −72.3 dB. The sound driver divides the result
/// by 16, 4 or 2 below −24, −12 and −6 dB, so each of those ranges is
/// scaled up to fill 7 bits. Derived from how the driver uses it, not from
/// the BIOS's own table, so rounding may differ by one.
fn volume_table(index: u32) -> u32 {
    let index = index.min(723);
    let shift = match index {
        0..483 => 3,
        483..603 => 2,
        603..663 => 1,
        _ => 0,
    };
    let decibels = (f64::from(index) - 723.0) / 10.0;
    let level = 127.0 * 10f64.powf(decibels / 20.0) * f64::from(1u32 << shift);
    (level.round() as u32).min(127)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 64 KiB of flat memory for exercising calls.
    struct Ram(Vec<u8>);

    impl Bus for Ram {
        fn read_32(&mut self, address: u32, _: Access) -> u32 {
            let i = (address & 0xFFFC) as usize;
            u32::from_le_bytes([self.0[i], self.0[i + 1], self.0[i + 2], self.0[i + 3]])
        }
        fn read_16(&mut self, address: u32, _: Access) -> u16 {
            let i = (address & 0xFFFE) as usize;
            u16::from_le_bytes([self.0[i], self.0[i + 1]])
        }
        fn read_8(&mut self, address: u32, _: Access) -> u8 {
            self.0[(address & 0xFFFF) as usize]
        }
        fn write_32(&mut self, address: u32, value: u32, _: Access) {
            let i = (address & 0xFFFC) as usize;
            self.0[i..i + 4].copy_from_slice(&value.to_le_bytes());
        }
        fn write_16(&mut self, address: u32, value: u16, _: Access) {
            let i = (address & 0xFFFE) as usize;
            self.0[i..i + 2].copy_from_slice(&value.to_le_bytes());
        }
        fn write_8(&mut self, address: u32, value: u8, _: Access) {
            self.0[(address & 0xFFFF) as usize] = value;
        }
        fn idle(&mut self) {}
    }

    fn call(side: Side, args: [u32; 4]) -> Call {
        Call {
            side,
            args,
            intr_flags: 0xFFF8,
        }
    }

    #[test]
    fn crc16_matches_the_standard_check_value() {
        assert_eq!(crc16(0, *b"123456789"), 0xBB3D);
    }

    #[test]
    fn intr_wait_halts_until_the_flag_is_set() {
        let mut ram = Ram(vec![0; 0x1_0000]);
        let effects = run(0x05, &call(Side::Arm7, [0; 4]), &mut ram);
        assert!(effects.halt && effects.repeat);
        ram.write_32(0xFFF8, 1, Access::NONSEQUENTIAL);
        let effects = run(0x04, &call(Side::Arm7, [0, 1, 0, 0]), &mut ram);
        assert!(!effects.halt);
        assert_eq!(ram.read_32(0xFFF8, Access::NONSEQUENTIAL), 0, "flag consumed");
    }

    #[test]
    fn lz77_expands_back_references() {
        let mut ram = Ram(vec![0; 0x1_0000]);
        // Header: type 0x10, 6 bytes. Flags 0b0100_0000: literal, then a
        // reference of length 5 at distance 1.
        let packed = [0x10, 6, 0, 0, 0b0100_0000, b'A', 0x20, 0x00];
        ram.0[0x100..0x108].copy_from_slice(&packed);
        run(0x11, &call(Side::Arm9, [0x100, 0x200, 0, 0]), &mut ram);
        assert_eq!(&ram.0[0x200..0x206], b"AAAAAA");
    }

    #[test]
    fn divide_returns_quotient_remainder_and_magnitude() {
        assert_eq!(
            divide(-7, 2),
            [(0, (-3i32) as u32), (1, (-1i32) as u32), (3, 3)]
        );
    }

    #[test]
    fn sound_tables_span_their_ranges() {
        assert_eq!(pitch_table(0), 0);
        assert!(pitch_table(767) > 0xFF00);
        assert_eq!(sine_table(0), 0);
        assert_eq!(volume_table(723), 127);
        assert_eq!(volume_table(663), 64);
        assert!(volume_table(0) < volume_table(180));
    }
}
