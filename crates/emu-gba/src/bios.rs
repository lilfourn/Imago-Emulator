//! A high-level emulation of the GBA BIOS (GBATEK, "BIOS Functions").
//!
//! No Nintendo code is used. The 16 KiB BIOS region holds a small vector
//! table written here: the IRQ vector runs a six-instruction dispatcher that
//! calls the game's handler at 0x03007FFC, as every GBA program expects;
//! the SWI vector leads to a handler that just returns. System calls never
//! reach that code:
//! the machine intercepts the CPU as it enters the SWI vector and runs the
//! function here in Rust instead.

use std::f64::consts::PI;

use emu_arm::{Access, Bus as _, Cpu};

use crate::bus::Bus;

/// The BIOS image: vector table and IRQ dispatcher.
pub(crate) fn image() -> Box<[u8]> {
    let mut image = vec![0u8; 0x4000];
    let mut put = |address: usize, instruction: u32| {
        image[address..address + 4].copy_from_slice(&instruction.to_le_bytes());
    };
    put(0x00, 0xEAFF_FFFE); // reset: B . (never used; boot skips the BIOS)
    put(0x04, 0xE1B0_F00E); // undefined: MOVS PC, LR
    put(0x08, 0xEA00_004C); // SWI: B 0x140 (calls are intercepted first)
    put(0x18, 0xEA00_0042); // IRQ: B 0x128
    put(0x1C, 0xE25E_F004); // FIQ: SUBS PC, LR, #4
    put(0x128, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    put(0x12C, 0xE3A0_0301); // MOV R0, #0x04000000
    put(0x130, 0xE28F_E000); // ADD LR, PC, #0
    put(0x134, 0xE510_F004); // LDR PC, [R0, #-4]  (handler at 0x03FFFFFC)
    put(0x138, 0xE8BD_500F); // LDMFD SP!, {R0-R3, R12, LR}
    put(0x13C, 0xE25E_F004); // SUBS PC, LR, #4
    // A SWI handler that reads the call number and returns. Calls are
    // handled in Rust before this runs, but its first instructions are what
    // the CPU prefetches after an IRQ returns, which BIOS reads then see.
    put(0x140, 0xE92D_5800); // STMFD SP!, {R11, R12, LR}
    put(0x144, 0xE55E_C002); // LDRB R12, [LR, #-2]
    put(0x148, 0xE8BD_5800); // LDMFD SP!, {R11, R12, LR}
    put(0x14C, 0xE1B0_F00E); // MOVS PC, LR
    image.into_boxed_slice()
}

/// What BIOS reads return from outside the BIOS just after boot and just
/// after a system call: the opcodes the real BIOS last fetched at those
/// points (GBATEK, "BIOS Memory Protection").
pub(crate) const OPEN_BUS_AFTER_BOOT: u32 = 0xE129_F000;
pub(crate) const OPEN_BUS_AFTER_SWI: u32 = 0xE3A0_2004;

/// The address the CPU's R15 holds once it has entered the SWI vector.
pub(crate) const SWI_ENTERED_PC: u32 = 0x08 + 8;

/// Runs the system call the CPU just entered, then returns from it as the
/// BIOS's `MOVS PC, LR` would.
pub(crate) fn call(cpu: &mut Cpu, bus: &mut Bus) {
    // The comment field of the SWI instruction picks the function.
    let return_address = cpu.reg(14);
    let thumb = cpu.saved_psr().is_some_and(|spsr| spsr & (1 << 5) != 0);
    let number = if thumb {
        bus.read_16(return_address.wrapping_sub(2), Access::NONSEQUENTIAL) as u8
    } else {
        (bus.read_32(return_address.wrapping_sub(4), Access::NONSEQUENTIAL) >> 16) as u8
    };
    let r = |i| cpu.reg(i);
    let (r0, r1, r2, r3) = (r(0), r(1), r(2), r(3));
    match number {
        0x00 => soft_reset(cpu, bus),
        0x01 => register_ram_reset(bus, r0),
        0x02 | 0x03 => bus.halt(),
        0x04 => return intr_wait(cpu, bus, r0 != 0, r1 as u16),
        0x05 => return intr_wait(cpu, bus, true, 1),
        0x06 => divide(cpu, r0 as i32, r1 as i32),
        0x07 => divide(cpu, r1 as i32, r0 as i32),
        0x08 => cpu.set_reg(0, isqrt(r0)),
        0x09 => cpu.set_reg(0, arc_tan(r0 as i16 as i32) as u16 as u32),
        0x0A => cpu.set_reg(0, u32::from(arc_tan2(r0 as i16 as i32, r1 as i16 as i32))),
        0x0B => cpu_set(bus, r0, r1, r2),
        0x0C => cpu_fast_set(bus, r0, r1, r2),
        0x0D => cpu.set_reg(0, 0xBAAE_187F),
        0x0E => bg_affine_set(bus, r0, r1, r2),
        0x0F => obj_affine_set(bus, r0, r1, r2, r3),
        0x10 => bit_unpack(bus, r0, r1, r2),
        0x11 => lz77(bus, r0, r1, false),
        0x12 => lz77(bus, r0, r1, true),
        0x13 => huffman(bus, r0, r1),
        0x14 => run_length(bus, r0, r1, false),
        0x15 => run_length(bus, r0, r1, true),
        0x16 => diff_unfilter(bus, r0, r1, false, false),
        0x17 => diff_unfilter(bus, r0, r1, false, true),
        0x18 => diff_unfilter(bus, r0, r1, true, true),
        0x19 => bus.write_16(
            0x0400_0088,
            if r0 != 0 { 0x200 } else { 0 },
            Access::NONSEQUENTIAL,
        ),
        0x1F => cpu.set_reg(0, midi_key_to_frequency(bus, r0, r1 as u8, r2 as u8)),
        // Sound driver, multiboot and the rest are not emulated.
        _ => {}
    }
    bus.set_bios_open_bus(OPEN_BUS_AFTER_SWI);
    cpu.return_from_exception(bus);
}

fn soft_reset(cpu: &mut Cpu, bus: &mut Bus) {
    let to_ram = bus.read_8(0x0300_7FFA, Access::NONSEQUENTIAL) != 0;
    for address in (0x0300_7E00..0x0300_8000).step_by(4) {
        bus.write_32(address, 0, Access::NONSEQUENTIAL);
    }
    let mut registers = cpu.registers();
    registers.svc[0] = 0x0300_7FE0;
    registers.irq[0] = 0x0300_7FA0;
    registers.usr[5] = 0x0300_7F00;
    registers.svc[1] = if to_ram { 0x0200_0000 } else { 0x0800_0000 };
    registers.spsr[1] = 0x1F;
    *cpu = Cpu::from_registers(&registers, cpu.pipeline(), cpu.fetch_access());
}

fn register_ram_reset(bus: &mut Bus, flags: u32) {
    let mut clear = |start: u32, length: u32| {
        for address in (start..start + length).step_by(4) {
            bus.write_32(address, 0, Access::NONSEQUENTIAL);
        }
    };
    if flags & 0x01 != 0 {
        clear(0x0200_0000, 0x4_0000);
    }
    if flags & 0x02 != 0 {
        // The top 0x200 bytes of IWRAM (stacks, IRQ vector) are kept.
        clear(0x0300_0000, 0x7E00);
    }
    if flags & 0x04 != 0 {
        clear(0x0500_0000, 0x400);
    }
    if flags & 0x08 != 0 {
        clear(0x0600_0000, 0x1_8000);
    }
    if flags & 0x10 != 0 {
        clear(0x0700_0000, 0x400);
    }
}

/// IntrWait: waits until one of `flags` is set in the BIOS interrupt flags
/// at 0x03007FF8 (which the game's IRQ handler sets), then clears it.
///
/// While waiting, the CPU halts and returns to the SWI instruction itself,
/// so the call runs again after each interrupt; R0 is zeroed so the repeat
/// does not discard flags the handler just set.
fn intr_wait(cpu: &mut Cpu, bus: &mut Bus, discard: bool, flags: u16) {
    const BIOS_FLAGS: u32 = 0x0300_7FF8;
    let mut current = bus.read_16(BIOS_FLAGS, Access::NONSEQUENTIAL);
    if discard {
        current &= !flags;
        bus.write_16(BIOS_FLAGS, current, Access::NONSEQUENTIAL);
    }
    bus.write_16(0x0400_0208, 1, Access::NONSEQUENTIAL);
    if current & flags != 0 {
        bus.write_16(BIOS_FLAGS, current & !flags, Access::NONSEQUENTIAL);
    } else {
        let thumb = cpu.saved_psr().is_some_and(|spsr| spsr & (1 << 5) != 0);
        let swi_length = if thumb { 2 } else { 4 };
        cpu.set_reg(0, 0);
        cpu.set_reg(14, cpu.reg(14).wrapping_sub(swi_length));
        bus.halt();
    }
    bus.set_bios_open_bus(OPEN_BUS_AFTER_SWI);
    cpu.return_from_exception(bus);
}

fn divide(cpu: &mut Cpu, numerator: i32, denominator: i32) {
    if denominator == 0 {
        // The real BIOS loops forever; return something harmless instead.
        cpu.set_reg(0, if numerator < 0 { u32::MAX } else { 1 });
        cpu.set_reg(1, numerator as u32);
        cpu.set_reg(3, 1);
        return;
    }
    let quotient = numerator.wrapping_div(denominator);
    cpu.set_reg(0, quotient as u32);
    cpu.set_reg(1, numerator.wrapping_rem(denominator) as u32);
    cpu.set_reg(3, quotient.unsigned_abs());
}

fn isqrt(value: u32) -> u32 {
    let mut root = (f64::from(value)).sqrt() as u32;
    while u64::from(root) * u64::from(root) > u64::from(value) {
        root -= 1;
    }
    while u64::from(root + 1) * u64::from(root + 1) <= u64::from(value) {
        root += 1;
    }
    root
}

/// ArcTan: the BIOS's polynomial approximation, with `tan` in 1.14 fixed
/// point, returning an angle where 0x4000 is 90 degrees.
fn arc_tan(tan: i32) -> i32 {
    let a = -((tan.wrapping_mul(tan)) >> 14);
    let mut b = ((0xA9 * a) >> 14) + 0x390;
    for constant in [0x91C, 0xFB6, 0x16AA, 0x2081, 0x3651, 0xA2F9] {
        b = (b.wrapping_mul(a) >> 14) + constant;
    }
    tan.wrapping_mul(b) >> 16
}

/// ArcTan2: the angle of (x, y) in 0..0x10000.
fn arc_tan2(x: i32, y: i32) -> u16 {
    let result = if y == 0 {
        if x >= 0 { 0 } else { 0x8000 }
    } else if x == 0 {
        if y >= 0 { 0x4000 } else { 0xC000 }
    } else if y >= 0 {
        if x >= 0 && x >= y {
            arc_tan((y << 14) / x)
        } else if x < 0 && -x >= y {
            arc_tan((y << 14) / x) + 0x8000
        } else {
            0x4000 - arc_tan((x << 14) / y)
        }
    } else if x <= 0 && -x > -y {
        arc_tan((y << 14) / x) + 0x8000
    } else if x > 0 && x >= -y {
        arc_tan((y << 14) / x) + 0x1_0000
    } else {
        0xC000 - arc_tan((x << 14) / y)
    };
    result as u16
}

/// CpuSet: copies or fills `control & 0x1FFFFF` halfwords or words.
fn cpu_set(bus: &mut Bus, source: u32, destination: u32, control: u32) {
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
fn cpu_fast_set(bus: &mut Bus, source: u32, destination: u32, control: u32) {
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

fn read_i16(bus: &mut Bus, address: u32) -> f64 {
    f64::from(bus.read_16(address, Access::NONSEQUENTIAL) as i16)
}

/// BgAffineSet: builds background rotation/scaling parameters.
fn bg_affine_set(bus: &mut Bus, mut source: u32, mut destination: u32, count: u32) {
    for _ in 0..count {
        let origin_x = f64::from(bus.read_32(source, Access::NONSEQUENTIAL) as i32) / 256.0;
        let origin_y = f64::from(bus.read_32(source + 4, Access::NONSEQUENTIAL) as i32) / 256.0;
        let center_x = read_i16(bus, source + 8);
        let center_y = read_i16(bus, source + 10);
        let scale_x = read_i16(bus, source + 12) / 256.0;
        let scale_y = read_i16(bus, source + 14) / 256.0;
        let angle = f64::from(bus.read_16(source + 16, Access::NONSEQUENTIAL) >> 8) / 128.0 * PI;
        let (sin, cos) = angle.sin_cos();
        let (pa, pb, pc, pd) = (cos * scale_x, -sin * scale_x, sin * scale_y, cos * scale_y);
        let x = origin_x - (pa * center_x + pb * center_y);
        let y = origin_y - (pc * center_x + pd * center_y);
        for (offset, value) in [(0, pa), (2, pb), (4, pc), (6, pd)] {
            bus.write_16(
                destination + offset,
                (value * 256.0) as i16 as u16,
                Access::NONSEQUENTIAL,
            );
        }
        bus.write_32(
            destination + 8,
            (x * 256.0) as i32 as u32,
            Access::NONSEQUENTIAL,
        );
        bus.write_32(
            destination + 12,
            (y * 256.0) as i32 as u32,
            Access::NONSEQUENTIAL,
        );
        source += 20;
        destination += 16;
    }
}

/// ObjAffineSet: builds object rotation/scaling parameters, writing each
/// of the four values `stride` bytes apart.
fn obj_affine_set(bus: &mut Bus, mut source: u32, mut destination: u32, count: u32, stride: u32) {
    for _ in 0..count {
        let scale_x = read_i16(bus, source) / 256.0;
        let scale_y = read_i16(bus, source + 2) / 256.0;
        let angle = f64::from(bus.read_16(source + 4, Access::NONSEQUENTIAL) >> 8) / 128.0 * PI;
        let (sin, cos) = angle.sin_cos();
        for value in [cos * scale_x, -sin * scale_x, sin * scale_y, cos * scale_y] {
            bus.write_16(
                destination,
                (value * 256.0) as i16 as u16,
                Access::NONSEQUENTIAL,
            );
            destination = destination.wrapping_add(stride);
        }
        source += 8;
    }
}

/// BitUnPack: widens packed pixels, adding an offset to non-zero (or all)
/// values.
fn bit_unpack(bus: &mut Bus, mut source: u32, mut destination: u32, info: u32) {
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

/// Writes decompressed bytes, either byte by byte (WRAM functions) or
/// a halfword at a time (VRAM functions, which cannot write single bytes).
struct Output {
    destination: u32,
    halfwords: bool,
    pending: Option<u8>,
}

impl Output {
    fn push(&mut self, bus: &mut Bus, byte: u8) {
        if !self.halfwords {
            bus.write_8(self.destination, byte, Access::NONSEQUENTIAL);
            self.destination += 1;
            return;
        }
        match self.pending.take() {
            None => self.pending = Some(byte),
            Some(low) => {
                bus.write_16(
                    self.destination,
                    u16::from_le_bytes([low, byte]),
                    Access::NONSEQUENTIAL,
                );
                self.destination += 2;
            }
        }
    }

    /// The byte written `distance` bytes back, for LZ77 back-references.
    fn earlier(&self, bus: &mut Bus, distance: u32) -> u8 {
        let written = self.destination + u32::from(self.pending.is_some());
        let address = written.wrapping_sub(distance);
        if self.halfwords && self.pending.is_some() && distance == 1 {
            return self.pending.unwrap_or(0);
        }
        bus.read_8(address, Access::NONSEQUENTIAL)
    }
}

/// LZ77UnComp: GBATEK's LZ77 format with a 4-byte header.
fn lz77(bus: &mut Bus, mut source: u32, destination: u32, halfwords: bool) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source += 4;
    let mut out = Output {
        destination,
        halfwords,
        pending: None,
    };
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
                out.push(bus, byte);
                remaining -= 1;
            } else {
                let high = u32::from(bus.read_8(source, Access::NONSEQUENTIAL));
                let low = u32::from(bus.read_8(source + 1, Access::NONSEQUENTIAL));
                source += 2;
                let length = (high >> 4) + 3;
                let distance = ((high & 0xF) << 8 | low) + 1;
                for _ in 0..length.min(remaining) {
                    let byte = out.earlier(bus, distance);
                    out.push(bus, byte);
                    remaining -= 1;
                }
            }
        }
    }
}

/// RLUnComp: runs of repeated bytes and literal stretches.
fn run_length(bus: &mut Bus, mut source: u32, destination: u32, halfwords: bool) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let mut remaining = header >> 8;
    source += 4;
    let mut out = Output {
        destination,
        halfwords,
        pending: None,
    };
    while remaining > 0 {
        let flag = bus.read_8(source, Access::NONSEQUENTIAL);
        source += 1;
        if flag & 0x80 == 0 {
            for _ in 0..(u32::from(flag & 0x7F) + 1).min(remaining) {
                let byte = bus.read_8(source, Access::NONSEQUENTIAL);
                source += 1;
                out.push(bus, byte);
                remaining -= 1;
            }
        } else {
            let byte = bus.read_8(source, Access::NONSEQUENTIAL);
            source += 1;
            for _ in 0..(u32::from(flag & 0x7F) + 3).min(remaining) {
                out.push(bus, byte);
                remaining -= 1;
            }
        }
    }
}

/// HuffUnComp: GBATEK's Huffman format with 4- or 8-bit symbols.
fn huffman(bus: &mut Bus, mut source: u32, mut destination: u32) {
    let header = bus.read_32(source, Access::NONSEQUENTIAL);
    let symbol_bits = header & 0xF;
    let mut remaining = header >> 8;
    let tree_size = u32::from(bus.read_8(source + 4, Access::NONSEQUENTIAL));
    let tree = source + 5;
    let root = tree;
    source = tree + tree_size * 2 + 1;
    let mut node = root;
    let mut node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
    let mut out = 0u32;
    let mut out_bits = 0;
    while remaining > 0 {
        let bits = bus.read_32(source, Access::NONSEQUENTIAL);
        source += 4;
        for bit in (0..32).rev() {
            let right = bits & (1 << bit) != 0;
            let next = (node & !1) + (node_value & 0x3F) * 2 + 2 + u32::from(right);
            let terminal = node_value & if right { 0x40 } else { 0x80 } != 0;
            node = next;
            node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
            if terminal {
                out |= node_value << out_bits;
                out_bits += symbol_bits;
                node = root;
                node_value = u32::from(bus.read_8(node, Access::NONSEQUENTIAL));
                if out_bits == 32 {
                    bus.write_32(destination, out, Access::NONSEQUENTIAL);
                    destination += 4;
                    out = 0;
                    out_bits = 0;
                    remaining = remaining.saturating_sub(4);
                    if remaining == 0 {
                        return;
                    }
                }
            }
        }
    }
}

/// Diff8bitUnFilter and Diff16bitUnFilter: running sums.
fn diff_unfilter(
    bus: &mut Bus,
    mut source: u32,
    mut destination: u32,
    sixteen: bool,
    halfwords: bool,
) {
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
    let mut out = Output {
        destination,
        halfwords,
        pending: None,
    };
    let mut sum = 0u8;
    while remaining > 0 {
        sum = sum.wrapping_add(bus.read_8(source, Access::NONSEQUENTIAL));
        out.push(bus, sum);
        source += 1;
        remaining -= 1;
    }
}

/// MidiKey2Freq: the sample rate that plays a wave at a MIDI key.
fn midi_key_to_frequency(bus: &mut Bus, wave: u32, key: u8, fine: u8) -> u32 {
    let base = f64::from(bus.read_32(wave + 4, Access::NONSEQUENTIAL));
    let exponent = (180.0 - f64::from(key) - f64::from(fine) / 256.0) / 12.0;
    (base / 2f64.powf(exponent)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_square_roots() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(15), 3);
        assert_eq!(isqrt(16), 4);
        assert_eq!(isqrt(u32::MAX), 65_535);
    }

    #[test]
    fn arc_tan2_quadrants() {
        assert_eq!(arc_tan2(1, 0), 0);
        assert_eq!(arc_tan2(0, 1), 0x4000);
        assert_eq!(arc_tan2(-1, 0), 0x8000);
        assert_eq!(arc_tan2(0, -1), 0xC000);
        let diagonal = arc_tan2(100, 100);
        assert!((0x1FF0..=0x2010).contains(&diagonal), "{diagonal:x}");
    }
}
