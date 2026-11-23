//! Each CPU's I/O registers at 0x04000000 (GBATEK, "DS I/O Map").
//!
//! # Writes
//!
//! A register has one writer, whatever the width of the write, and it is
//! told which bytes the write covers by a mask. A register that is a whole
//! word (a DMA channel's, ROMCTRL, the maths unit's, the 3D engine's) is
//! written by [`Hardware::write_word`] with the value in its place in the
//! word; every other register is a halfword, written by
//! [`Hardware::write_half`], which also hands a word register its part. A
//! word written to two halfword registers is the low one, then the high.
//!
//! Either way a register changes only the bits the mask covers and keeps
//! the rest as it holds them itself, which it is asked for only when the
//! write is narrower than it. So a byte written beside a bit the hardware
//! changed, or beside one that only acts when written, leaves that bit
//! alone. The registers with rules of their own say so where they are
//! written: IF clears the ones written, POSTFLG, HALTCNT and VRAMCNT are
//! bytes, the IPC FIFO's send port takes only words.
//!
//! # The raw copy
//!
//! `io9` and `io7` hold what the rules below keep of the writes to each
//! offset of the page: a word to a DMA, ROMCTRL, fill or maths register
//! is not kept, nor a byte to IF, VRAMCNT, the 3D engine, sound, or the
//! ARM7's POSTFLG and HALTCNT.
//! An offset with no register behind it is read back from there, which is
//! all DISPCAPCNT and the ARM7's EXMEMSTAT bits are. For every other
//! register the copy is never read: it stays because it is part of a save
//! state, and it is kept exactly as it always was ([`Hardware::keeps`],
//! [`Hardware::keep_beside_device`]), so a state has the same bytes.

use crate::dma::Timing;
use crate::hardware::{Event, Hardware};
use crate::interrupt::irq;
use crate::ipc::Side;

/// Registers that live in the ARM9's or ARM7's 0x04000000 page.
const IO_PAGE: u32 = 0x1FFF;

/// The first address past the page. Above it are only the IPC FIFO's and
/// the game card's read ports; the page is not mirrored there.
const PAGE_END: u32 = 0x0400_2000;

/// The IPCFIFOCNT bits that are settings a write replaces: the two
/// interrupt enables and the FIFOs' enable. The rest are status, or act
/// when a one is written and are not kept.
const FIFO_SETTINGS: u16 = 0x8404;

/// `old` with the bits `mask` covers replaced by `value`'s.
fn merge(old: u32, value: u32, mask: u32) -> u32 {
    (old & !mask) | (value & mask)
}

/// What a halfword register is written with: `value` whole, or, when
/// `mask` covers only one of its bytes, what it holds with that byte
/// replaced. The register is asked what it holds only then.
fn merged(mask: u16, value: u16, held: impl FnOnce() -> u16) -> u16 {
    if mask == 0xFFFF {
        value
    } else {
        (held() & !mask) | (value & mask)
    }
}

/// The same for a register that is a whole word.
fn merged_word(mask: u32, value: u32, held: impl FnOnce() -> u32) -> u32 {
    if mask == u32::MAX {
        value
    } else {
        merge(held(), value, mask)
    }
}

/// The VRAM bank (A-I as 0-8) whose VRAMCNT byte is at `offset`:
/// 0x240-0x246, then 0x248-0x249 past WRAMCNT.
fn vram_bank(offset: u32) -> usize {
    let bank = if offset < 0x247 {
        offset - 0x240
    } else {
        offset - 0x241
    };
    bank as usize
}

impl Hardware {
    fn raw(&self, side: Side) -> &[u8] {
        match side {
            Side::Arm9 => &self.io9,
            Side::Arm7 => &self.io7,
        }
    }

    fn raw_mut(&mut self, side: Side) -> &mut [u8] {
        match side {
            Side::Arm9 => &mut self.io9,
            Side::Arm7 => &mut self.io7,
        }
    }

    pub(crate) fn raw_16(&self, side: Side, offset: u32) -> u16 {
        let index = (offset & IO_PAGE & !1) as usize;
        let bytes = &self.raw(side)[index..index + 2];
        u16::from_le_bytes([bytes[0], bytes[1]])
    }

    /// Keeps the bytes of `value` that `mask` covers in the raw copy of
    /// the halfword at `offset`.
    fn keep_half(&mut self, side: Side, offset: u32, value: u16, mask: u16) {
        let index = (offset & IO_PAGE & !1) as usize;
        let bytes = value.to_le_bytes();
        let raw = self.raw_mut(side);
        match mask {
            0xFFFF => raw[index..index + 2].copy_from_slice(&bytes),
            0x00FF => raw[index] = bytes[0],
            _ => raw[index + 1] = bytes[1],
        }
    }

    /// Whether the raw copy keeps a write to the halfword at `offset` that
    /// covers the bytes in `mask`. It keeps every halfword written but the
    /// ARM7's POSTFLG and HALTCNT, and most bytes: the registers that are
    /// bytes, or that take bytes without their neighbours, never had a
    /// byte write kept.
    fn keeps(side: Side, offset: u32, mask: u16) -> bool {
        if mask == 0xFFFF {
            return !(offset == 0x300 && side == Side::Arm7);
        }
        match (side, offset) {
            // IF; the ARM7's POSTFLG and HALTCNT, and its sound registers;
            // the ARM9's VRAMCNT and WRAMCNT, and its 3D engine's.
            (_, 0x214 | 0x216)
            | (Side::Arm7, 0x300 | 0x400..=0x51F)
            | (Side::Arm9, 0x240..=0x249 | 0x060..=0x063 | 0x320..=0x6A3) => false,
            // The ARM9's POSTFLG, the low byte.
            (Side::Arm9, 0x300) => mask != 0x00FF,
            _ => true,
        }
    }

    /// The raw copy of a byte written to a register that is a word kept
    /// by its device (a DMA channel's, ROMCTRL, and the ARM9's DMA fill
    /// values and maths unit): the halfword the byte is in is kept as the
    /// CPU read it just before, with the byte put in. (A halfword written
    /// to one is kept as written, as for any register, and a word is not
    /// kept: it goes to the device alone.)
    fn keep_beside_device(&mut self, side: Side, offset: u32, value: u32, mask: u32) {
        if mask.count_ones() != 8 {
            return;
        }
        let half = offset | (mask.trailing_zeros() / 16 * 2);
        let shift = (half & 2) * 8;
        let read = u32::from(self.page_16(side, half)) << shift;
        let kept = merge(read, value, mask) >> shift;
        self.keep_half(side, half, kept as u16, 0xFFFF);
    }

    /// The registers cheat codes test, read without side effects: the
    /// ARM7's timers (0x04000100–0x0400010F), KEYINPUT (0x04000130) and
    /// EXTKEYIN (0x04000136). The ARM7's, because the Action Replay runs
    /// there. `None` for any other address.
    pub(crate) fn peek_cheat_register(&self, address: u32) -> Option<u8> {
        let half = match address & !1 {
            0x0400_0100..=0x0400_010E => {
                let offset = (address & !1) - 0x0400_0100;
                self.arm7.timers.read(offset, self.now(Side::Arm7))
            }
            0x0400_0130 => self.keys,
            0x0400_0136 => self.ext_keys,
            _ => return None,
        };
        Some((half >> ((address & 1) * 8)) as u8)
    }

    /// The GBA-slot timing bits (EXMEMCNT bits 0-6) as `side` last wrote
    /// them: each CPU keeps its own.
    pub(crate) fn gba_slot_timing(&self, side: Side) -> u16 {
        match side {
            Side::Arm9 => self.exmemcnt & 0x7F,
            Side::Arm7 => self.raw_16(Side::Arm7, 0x204) & 0x7F,
        }
    }

    /// DISPSTAT as `side` reads it.
    fn dispstat(&self, side: Side) -> u16 {
        let stored = self.cpu(side).dispstat;
        let setting = (stored >> 8) | (stored & 0x80) << 1;
        let vblank = (192..262).contains(&self.vcount);
        u16::from(vblank)
            | u16::from(self.hblank) << 1
            | u16::from(self.vcount == setting) << 2
            | (stored & 0xFFB8)
    }

    /// Reads one of the registers both CPUs have at the same offset.
    #[inline]
    fn read_common_16(&self, side: Side, offset: u32) -> Option<u16> {
        Some(match offset {
            0x004 => self.dispstat(side),
            0x006 => self.vcount,
            0x0B0..=0x0DF => {
                let value = self.cpu(side).dma.read((offset - 0xB0) & !3);
                (value >> ((offset & 2) * 8)) as u16
            }
            0x100..=0x10F => {
                let now = self.now(side);
                self.cpu(side).timers.read(offset - 0x100, now)
            }
            0x130 => self.keys,
            0x132 => self.cpu(side).keycnt,
            0x180 => self.ipc.read_sync(side),
            0x184 => self.ipc.read_control(side),
            0x208 => u16::from(self.cpu(side).interrupts.master_enable),
            0x210 => self.cpu(side).interrupts.enable as u16,
            0x212 => (self.cpu(side).interrupts.enable >> 16) as u16,
            0x214 => self.cpu(side).interrupts.flags as u16,
            0x216 => (self.cpu(side).interrupts.flags >> 16) as u16,
            // The game card belongs to one CPU at a time; the other reads
            // nothing.
            0x1A0..=0x1BB if self.cart_owner() != side => 0,
            0x1A0 => self.cart.spi_control(),
            0x1A2 => u16::from(self.cart.spi_data()),
            0x1A4 => self.cart.rom_control() as u16,
            0x1A6 => (self.cart.rom_control() >> 16) as u16,
            // Command bytes and encryption seeds are write-only.
            0x1A8..=0x1BB => 0,
            _ => return None,
        })
    }

    /// Raises the keypad interrupt on each CPU whose KEYCNT asks for it.
    pub(crate) fn check_keypad_interrupt(&mut self) {
        for side in [Side::Arm9, Side::Arm7] {
            let keycnt = self.cpu(side).keycnt;
            if keycnt & (1 << 14) == 0 {
                continue;
            }
            let selected = keycnt & 0x03FF;
            let pressed = !self.keys & 0x03FF & selected;
            let fire = if keycnt & (1 << 15) != 0 {
                pressed == selected
            } else {
                pressed != 0
            };
            if fire {
                self.cpu_mut(side).interrupts.request(irq::KEYPAD);
            }
        }
    }

    // Reads.

    /// Reads a 32-bit register that is not two 16-bit halves.
    fn read_word(&mut self, side: Side, address: u32) -> Option<u32> {
        match address {
            0x0410_0000 => {
                let (value, raised) = self.ipc.receive(side);
                self.raise(raised);
                Some(value)
            }
            0x0410_0010 if self.cart_owner() == side => {
                let (value, event) = self.cart.read_data();
                self.rom_event(event);
                Some(value)
            }
            0x0410_0010 => Some(0),
            0x0400_00B0..=0x0400_00DF => {
                Some(self.cpu(side).dma.read((address - 0x0400_00B0) & !3))
            }
            0x0400_00E0..=0x0400_00EF if side == Side::Arm9 => {
                Some(self.arm9.dma.fill[((address >> 2) & 3) as usize])
            }
            0x0400_0280..=0x0400_02BF if side == Side::Arm9 => {
                Some(self.math.read(address - 0x0400_0280))
            }
            0x0400_0060 | 0x0400_0320..=0x0400_06A3 if side == Side::Arm9 => {
                Some(self.gpu3d.read_register(address & 0xFFF))
            }
            _ => None,
        }
    }

    /// `side` reads the word at `address`.
    pub(crate) fn io_read_32(&mut self, side: Side, address: u32) -> u32 {
        if let Some(value) = self.read_word(side, address) {
            return value;
        }
        u32::from(self.io_read_16(side, address))
            | u32::from(self.io_read_16(side, address + 2)) << 16
    }

    /// `side` reads the halfword at `address`.
    pub(crate) fn io_read_16(&mut self, side: Side, address: u32) -> u16 {
        if address >= PAGE_END {
            // The IPC FIFO and card data ports are words.
            let word = self.read_word(side, address & !3).unwrap_or(0);
            return (word >> ((address & 2) * 8)) as u16;
        }
        self.page_16(side, address)
    }

    /// `side` reads the byte at `address`.
    pub(crate) fn io_read_8(&mut self, side: Side, address: u32) -> u8 {
        if address >= PAGE_END {
            return (self.io_read_16(side, address & !1) >> ((address & 1) * 8)) as u8;
        }
        self.page_8(side, address)
    }

    /// Reads a register halfword in `side`'s 0x04000000 page, where no
    /// read has side effects.
    fn page_16(&self, side: Side, address: u32) -> u16 {
        match side {
            Side::Arm9 => self.arm9_page_16(address),
            Side::Arm7 => self.arm7_page_16(address),
        }
    }

    /// Reads a register byte in `side`'s 0x04000000 page.
    fn page_8(&self, side: Side, address: u32) -> u8 {
        match side {
            Side::Arm9 => self.arm9_page_8(address),
            Side::Arm7 => (self.arm7_page_16(address & !1) >> ((address & 1) * 8)) as u8,
        }
    }

    #[inline]
    pub(crate) fn arm9_page_16(&self, address: u32) -> u16 {
        let offset = address & IO_PAGE;
        if let Some(value) = self.read_common_16(Side::Arm9, offset) {
            return value;
        }
        match offset {
            0x060..=0x063 | 0x320..=0x6A3 => {
                (self.gpu3d.read_register(offset & !3) >> ((offset & 2) * 8)) as u16
            }
            0x000..=0x05F | 0x06C..=0x06F => self.engine_a.read_register(offset),
            0x0E0..=0x0EF => {
                let fill = self.arm9.dma.fill[((offset >> 2) & 3) as usize];
                (fill >> ((offset & 2) * 8)) as u16
            }
            0x1000..=0x106F => self.engine_b.read_register(offset - 0x1000),
            0x204 => self.exmemcnt,
            0x240..=0x249 => {
                u16::from(self.arm9_page_8(address)) | u16::from(self.arm9_page_8(address + 1)) << 8
            }
            0x280..=0x2BF => (self.math.read((offset - 0x280) & !3) >> ((offset & 2) * 8)) as u16,
            0x300 => u16::from(self.arm9.postflg),
            0x304 => self.powcnt1,
            _ => self.raw_16(Side::Arm9, offset),
        }
    }

    fn arm9_page_8(&self, address: u32) -> u8 {
        let offset = address & IO_PAGE;
        match offset {
            0x240..=0x246 | 0x248 | 0x249 => self.vram.control(vram_bank(offset)),
            0x247 => self.wramcnt,
            _ => (self.arm9_page_16(address & !1) >> ((address & 1) * 8)) as u8,
        }
    }

    #[inline]
    pub(crate) fn arm7_page_16(&self, address: u32) -> u16 {
        let offset = address & IO_PAGE;
        if let Some(value) = self.read_common_16(Side::Arm7, offset) {
            return value;
        }
        match offset {
            0x400..=0x51F => (self.sound.read(offset & !3 & 0x1FF) >> ((offset & 2) * 8)) as u16,
            0x136 => self.ext_keys,
            0x138 => self.rtc.read(),
            0x1C0 => self.spi.control(),
            0x1C2 => u16::from(self.spi.data()),
            // EXMEMSTAT: the ARM7's GBA-slot timing bits, then the ARM9's
            // EXMEMCNT.
            0x204 => (self.raw_16(Side::Arm7, 0x204) & 0x7F) | (self.exmemcnt & 0xFF80),
            0x240 => u16::from(self.vram.arm7_status()) | u16::from(self.wramcnt) << 8,
            0x300 => u16::from(self.arm7.postflg),
            0x304 => self.powcnt2,
            _ => self.raw_16(Side::Arm7, offset),
        }
    }

    // Writes.

    /// `side` writes the word at `address`.
    pub(crate) fn io_write_32(&mut self, side: Side, address: u32, value: u32) {
        // Nothing above the page can be written: the ports there are read.
        if address >= PAGE_END {
            return;
        }
        let offset = address & IO_PAGE;
        if self.write_word(side, offset, value, u32::MAX) {
            return;
        }
        // Two halfword registers: the low one first, as a word written to
        // a timer sets its reload before its control starts it.
        self.write_half(side, offset, value as u16, 0xFFFF);
        self.write_half(side, offset | 2, (value >> 16) as u16, 0xFFFF);
    }

    /// `side` writes the halfword at `address`.
    pub(crate) fn io_write_16(&mut self, side: Side, address: u32, value: u16) {
        if address >= PAGE_END {
            return;
        }
        self.write_half(side, address & IO_PAGE, value, 0xFFFF);
    }

    /// `side` writes the byte at `address`.
    pub(crate) fn io_write_8(&mut self, side: Side, address: u32, value: u8) {
        if address >= PAGE_END {
            return;
        }
        let shift = (address & 1) * 8;
        self.write_half(
            side,
            address & IO_PAGE & !1,
            u16::from(value) << shift,
            0xFF << shift,
        );
    }

    /// Writes the bytes `mask` covers (one, two or all four) of the word
    /// at `offset`, if a register that is a whole word is there. Returns
    /// whether one is.
    fn write_word(&mut self, side: Side, offset: u32, value: u32, mask: u32) -> bool {
        let arm9 = side == Side::Arm9;
        match offset {
            0x0B0..=0x0DF => {
                self.keep_beside_device(side, offset, value, mask);
                let register = offset - 0xB0;
                let dma = &mut self.cpu_mut(side).dma;
                let merged = merged_word(mask, value, || dma.read(register));
                dma.write(register, merged);
            }
            // IPCFIFOSEND takes whole words: anything less sends nothing.
            0x188 => {
                if mask == u32::MAX {
                    let raised = self.ipc.send(side, value);
                    self.raise(raised);
                }
            }
            // ROMCTRL, from the CPU the game card belongs to.
            0x1A4 if self.cart_owner() == side => {
                self.keep_beside_device(side, offset, value, mask);
                let merged = merged_word(mask, value, || self.cart.rom_control());
                let event = self.cart.write_rom_control(merged);
                self.rom_event(event);
            }
            // From the other CPU it reaches nothing; a word from it is
            // kept, as a write to an offset with no register is.
            0x1A4 if mask == u32::MAX => {
                self.keep_half(side, offset, value as u16, 0xFFFF);
                self.keep_half(side, offset | 2, (value >> 16) as u16, 0xFFFF);
            }
            0x1A4 => self.keep_beside_device(side, offset, value, mask),
            0x0E0..=0x0EF if arm9 => {
                self.keep_beside_device(side, offset, value, mask);
                let fill = &mut self.arm9.dma.fill[((offset >> 2) & 3) as usize];
                *fill = merge(*fill, value, mask);
            }
            0x280..=0x2BF if arm9 => {
                self.keep_beside_device(side, offset, value, mask);
                let register = offset - 0x280;
                let merged = merged_word(mask, value, || self.math.read(register));
                self.math.write(register, merged);
            }
            // The 3D engine takes the bytes written itself.
            0x060 | 0x320..=0x6A3 if arm9 => self.gpu3d.write(offset, value, mask),
            _ => return false,
        }
        true
    }

    /// A halfword or a byte written to a register that is a whole word:
    /// its place in the word, for [`Self::write_word`].
    fn write_word_part(&mut self, side: Side, offset: u32, value: u16, mask: u16) {
        let shift = (offset & 2) * 8;
        self.write_word(
            side,
            offset & !3,
            u32::from(value) << shift,
            u32::from(mask) << shift,
        );
    }

    /// Writes the bytes `mask` covers (0x00FF, 0xFF00 or both) of the
    /// halfword at `offset`: to the halfword register there, or to its
    /// part of a register that is a word.
    fn write_half(&mut self, side: Side, offset: u32, value: u16, mask: u16) {
        if Self::keeps(side, offset, mask) {
            self.keep_half(side, offset, value, mask);
        }
        match offset {
            0x004 => {
                let io = self.cpu_mut(side);
                io.dispstat = merged(mask, value, || io.dispstat) & 0xFFB8;
            }
            // The DMA channels', IPCFIFOSEND and ROMCTRL are words.
            0x0B0..=0x0DF | 0x188..=0x18B | 0x1A4..=0x1A7 => {
                self.write_word_part(side, offset, value, mask);
            }
            0x100..=0x10F => {
                let index = ((offset - 0x100) / 4) as usize;
                if offset.is_multiple_of(4) {
                    let timers = &mut self.cpu_mut(side).timers;
                    let reload = merged(mask, value, || timers.reload(index));
                    timers.write_reload(index, reload);
                } else {
                    let now = self.now(side);
                    let timers = &mut self.cpu_mut(side).timers;
                    let control = merged(mask, value, || timers.control(index));
                    let requested = timers.write_control(index, control, now);
                    self.request_timer_interrupts(side, requested);
                    self.schedule_timer(side, index);
                }
            }
            0x132 => {
                let io = self.cpu_mut(side);
                io.keycnt = merged(mask, value, || io.keycnt);
                self.check_keypad_interrupt();
            }
            // IPCSYNC: bit 13 sends the interrupt when it is written; it
            // does not read back, so nothing else sends it again.
            0x180 => {
                let sync = merged(mask, value, || self.ipc.read_sync(side));
                let raised = self.ipc.write_sync(side, sync);
                self.raise(raised);
            }
            // IPCFIFOCNT: its settings are kept; bit 3 flushes and bit 14
            // acknowledges an error only when a one is written to them.
            0x184 => {
                let control = merged(mask, value, || self.ipc.read_control(side) & FIFO_SETTINGS);
                let raised = self.ipc.write_control(side, control);
                self.raise(raised);
            }
            // IME is bit 0.
            0x208 => {
                if mask & 1 != 0 {
                    self.cpu_mut(side).interrupts.master_enable = value & 1 != 0;
                }
            }
            0x210 | 0x212 => {
                let shift = (offset & 2) * 8;
                let interrupts = &mut self.cpu_mut(side).interrupts;
                interrupts.enable = merge(
                    interrupts.enable,
                    u32::from(value) << shift,
                    u32::from(mask) << shift,
                );
            }
            // IF: only the ones written clear flags.
            0x214 | 0x216 => self
                .cpu_mut(side)
                .interrupts
                .acknowledge(u32::from(value & mask) << ((offset & 2) * 8)),
            // The game card belongs to one CPU at a time; the other's
            // writes reach nothing.
            0x1A0..=0x1BB => {
                if self.cart_owner() == side {
                    self.write_cart_half(side, offset, value, mask);
                }
            }
            _ => match side {
                Side::Arm9 => self.write_arm9_half(offset, value, mask),
                Side::Arm7 => self.write_arm7_half(offset, value, mask),
            },
        }
    }

    /// The game card's halfword registers, from the CPU it belongs to.
    /// (ROMCTRL is a word: [`Self::write_word`].)
    fn write_cart_half(&mut self, side: Side, offset: u32, value: u16, mask: u16) {
        match offset {
            0x1A0 => {
                let control = merged(mask, value, || self.cart.spi_control());
                self.cart.write_spi_control(control);
            }
            // AUXSPIDATA is the low byte: writing it sends it.
            0x1A2 => {
                if mask & 0x00FF != 0
                    && let Some(cycles) = self.cart.write_spi_data(value as u8)
                {
                    self.schedule_after(side, cycles, Event::CartSpiDone);
                }
            }
            // The command's eight bytes.
            0x1A8..=0x1AF => {
                let index = (offset - 0x1A8) as usize;
                if mask & 0x00FF != 0 {
                    self.cart.write_command(index, value as u8);
                }
                if mask & 0xFF00 != 0 {
                    self.cart.write_command(index + 1, (value >> 8) as u8);
                }
            }
            // The encryption seeds are not used.
            _ => {}
        }
    }

    /// The halfword registers only the ARM9 has, and its registers that
    /// are words.
    fn write_arm9_half(&mut self, offset: u32, value: u16, mask: u16) {
        match offset {
            // The 3D engine's, the DMA fill values and the maths unit
            // are words.
            0x060..=0x063 | 0x320..=0x6A3 | 0x0E0..=0x0EF | 0x280..=0x2BF => {
                self.write_word_part(Side::Arm9, offset, value, mask);
            }
            0x000..=0x05F | 0x06C..=0x06F => {
                self.engine_a.write_register_bytes(offset, value, mask);
            }
            0x1000..=0x106F => {
                self.engine_b
                    .write_register_bytes(offset - 0x1000, value, mask);
            }
            0x204 => self.exmemcnt = merged(mask, value, || self.exmemcnt),
            // VRAMCNT and WRAMCNT are bytes, the lower address first.
            0x240..=0x249 => {
                if mask & 0x00FF != 0 {
                    self.write_vram_control(offset, value as u8);
                }
                if mask & 0xFF00 != 0 {
                    self.write_vram_control(offset + 1, (value >> 8) as u8);
                }
            }
            // POSTFLG is the low byte. Bit 0, once set, stays set, as on
            // the ARM7; bit 1 is the ARM9's to set and clear.
            0x300 => {
                if mask & 0x00FF != 0 {
                    self.arm9.postflg = (value & 3) as u8 | (self.arm9.postflg & 1);
                }
            }
            0x304 => self.powcnt1 = merged(mask, value, || self.powcnt1) & 0x820F,
            _ => {}
        }
    }

    /// Writes the VRAMCNT or WRAMCNT byte at `offset`.
    fn write_vram_control(&mut self, offset: u32, value: u8) {
        match offset {
            0x247 => self.wramcnt = value & 3,
            0x240..=0x249 => self.vram.set_control(vram_bank(offset), value),
            _ => {}
        }
    }

    /// The halfword registers only the ARM7 has.
    fn write_arm7_half(&mut self, offset: u32, value: u16, mask: u16) {
        match offset {
            0x138 => {
                let now = self.now(Side::Arm7);
                let pins = merged(mask, value, || self.rtc.pins());
                self.rtc.write(pins, now);
            }
            0x1C0 => {
                let control = merged(mask, value, || self.spi.control());
                self.spi.write_control(control);
            }
            // SPIDATA is the low byte: writing it sends it.
            0x1C2 => {
                if mask & 0x00FF != 0 {
                    let now = self.now(Side::Arm7);
                    if let Some(cycles) = self.spi.write_data(value as u8, now) {
                        self.schedule_after(Side::Arm7, cycles, Event::SpiDone);
                    }
                }
            }
            // Two bytes. POSTFLG can only be set, and only from the BIOS;
            // HALTCNT above it halts on 2 and sleeps on 3 (treated as
            // halting).
            0x300 => {
                if mask & 0x00FF != 0 {
                    self.arm7.postflg |= (value & 1) as u8;
                }
                if mask & 0xFF00 != 0 && value >> 14 >= 2 {
                    self.arm7.halted = true;
                }
            }
            0x304 => {
                self.powcnt2 = merged(mask, value, || self.powcnt2) & 3;
                let now = self.now(Side::Arm7);
                self.wifi.set_enabled(self.powcnt2 & 2 != 0, now);
                self.sync_wifi();
            }
            // Sound registers are words that take the bytes written.
            0x400..=0x51F => {
                let shift = (offset & 2) * 8;
                self.sound.write(
                    offset & !3 & 0x1FF,
                    u32::from(value) << shift,
                    u32::from(mask) << shift,
                );
            }
            _ => {}
        }
    }

    /// The register byte at `address` as `side` reads it, without side
    /// effects, for debuggers: anything in the 0x04000000 page, but not
    /// the IPC FIFO or game card data ports at 0x04100000, whose reads
    /// take a word out.
    pub(crate) fn peek_io(&self, side: Side, address: u32) -> Option<u8> {
        if address >= PAGE_END {
            return None;
        }
        Some(self.page_8(side, address))
    }

    /// Starts DMA channels on both CPUs waiting for `timing`.
    pub(crate) fn trigger_dma(&mut self, timing: Timing) {
        self.arm9.dma.trigger(timing);
        self.arm7.dma.trigger(timing);
    }
}

#[cfg(test)]
mod widths;

#[cfg(test)]
mod tests {
    use crate::hardware::Hardware;
    use crate::ipc::Side;

    fn hardware() -> Hardware {
        Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into())
    }

    #[test]
    fn the_maths_unit_takes_halfword_control_writes() {
        let mut hw = hardware();
        hw.arm9_write_16(0x0400_02B0, 1); // SQRTCNT: 64-bit input
        hw.arm9_write_32(0x0400_02B8, 0);
        hw.arm9_write_32(0x0400_02BC, 1);
        assert_eq!(hw.arm9_read_32(0x0400_02B4), 65_536);
        assert_eq!(hw.arm9_read_16(0x0400_02B0), 1);
    }

    /// What libnds's `dmaFillHalfWords` does: the value into the fill
    /// register as a word, then a halfword DMA with a fixed source there.
    /// The halfwords read were the shadow's zeros.
    #[test]
    fn a_halfword_dma_from_the_fill_register_fills_with_its_value() {
        for (units, control, expected) in [
            // Enabled, source fixed, four halfwords.
            (
                2,
                0x8100_0004u32,
                [0x34, 0x12, 0x34, 0x12, 0x34, 0x12, 0x34, 0x12],
            ),
            // The same in two words.
            (
                4,
                0x8500_0002,
                [0x34, 0x12, 0x00, 0x00, 0x34, 0x12, 0x00, 0x00],
            ),
        ] {
            let mut hw = hardware();
            hw.arm9_write_32(0x0400_00EC, 0x1234);
            hw.arm9_write_32(0x0400_00D4, 0x0400_00EC); // DMA3SAD
            hw.arm9_write_32(0x0400_00D8, 0x0200_0000); // DMA3DAD
            hw.arm9_write_32(0x0400_00DC, control);
            assert!(hw.run_dma(Side::Arm9), "the DMA runs");
            let filled = [0, 1, 2, 3, 4, 5, 6, 7].map(|byte| hw.arm9_read_8(0x0200_0000 + byte));
            assert_eq!(filled, expected, "{units}-byte units");
        }
    }

    /// A byte write rebuilt its halfword from the shadow, which a 32-bit
    /// write never reached: writing DMA0CNT's top byte wiped the count's
    /// top bits and the address controls in the byte below it.
    #[test]
    fn a_byte_write_keeps_what_a_32_bit_write_stored() {
        let mut hw = hardware();
        // DMA0CNT: not enabled, destination fixed, source decrementing,
        // 0x12345 units.
        hw.arm9_write_32(0x0400_00B8, 0x00C1_2345);
        hw.arm9_write_8(0x0400_00BB, 0x04); // 32-bit units
        assert_eq!(hw.arm9_read_32(0x0400_00B8), 0x04C1_2345);
        hw.arm9_write_8(0x0400_00B9, 0xFF);
        assert_eq!(hw.arm9_read_32(0x0400_00B8), 0x04C1_FF45);
    }

    /// A finished DMA has cleared its enable bit, which the shadow still
    /// had set: a byte written beside it must not start the DMA again.
    #[test]
    fn a_byte_write_beside_a_finished_dma_does_not_start_it_again() {
        let mut hw = hardware();
        hw.arm9_write_32(0x0400_00B0, 0x0200_0100);
        hw.arm9_write_32(0x0400_00B4, 0x0200_0200);
        hw.arm9_write_16(0x0400_00B8, 1);
        hw.arm9_write_16(0x0400_00BA, 0x8000); // enable, at once
        assert!(hw.run_dma(Side::Arm9));
        assert_eq!(hw.arm9_read_32(0x0400_00B8) >> 31, 0, "finished");
        hw.arm9_write_8(0x0400_00BA, 0x60); // destination fixed
        assert_eq!(hw.arm9_read_32(0x0400_00B8), 0x0060_0001);
        assert_eq!(hw.arm9.dma.next_pending(), None);
    }
}
