//! VRAM banks A–I and where VRAMCNT maps them (GBATEK, "DS Memory Control
//! - VRAM").
//!
//! The DS has nine VRAM banks (656 KiB in all) and no fixed VRAM layout:
//! each bank's VRAMCNT register maps it to the LCDC area, a 2D engine's
//! background or sprite memory, the ARM7, 3D texture or texture-palette
//! slots, or extended-palette slots. Several banks may overlap; reads then
//! OR their data together and writes reach all of them.
//!
//! Every place a bank can go is a [`Region`] with an offset. Each region is
//! split into 8 KiB pages, and each page records which banks cover it. CPU
//! addresses, the 2D engines ([`EngineMemory`]) and the 3D engine all read
//! through regions.

use serde::{Deserialize, Serialize};

/// Bytes per page: the smallest mapping unit (one extended-palette slot).
const PAGE: u32 = 0x2000;

/// Bank sizes, A to I.
const BANK_SIZES: [u32; 9] = [
    0x2_0000, 0x2_0000, 0x2_0000, 0x2_0000, 0x1_0000, 0x4000, 0x4000, 0x8000, 0x4000,
];

/// Each bank's fixed place in the LCDC area (0x06800000 on the ARM9).
const LCDC_OFFSETS: [u32; 9] = [
    0x0_0000, 0x2_0000, 0x4_0000, 0x6_0000, 0x8_0000, 0x9_0000, 0x9_4000, 0x9_8000, 0xA_0000,
];

/// Somewhere a bank can be mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Region {
    /// Banks seen directly by the ARM9 at 0x06800000.
    Lcdc,
    /// Engine A backgrounds, 0x06000000, 512 KiB.
    BgA,
    /// Engine A sprites, 0x06400000, 256 KiB.
    ObjA,
    /// Engine B backgrounds, 0x06200000, 128 KiB.
    BgB,
    /// Engine B sprites, 0x06600000, 128 KiB.
    ObjB,
    /// ARM7 memory at 0x06000000, two 128 KiB slots.
    Arm7,
    /// 3D textures, four 128 KiB slots.
    Texture,
    /// 3D texture palettes, six 16 KiB slots.
    TexturePalette,
    /// Engine A extended background palettes, four 8 KiB slots.
    BgExtPaletteA,
    /// Engine B extended background palettes, four 8 KiB slots.
    BgExtPaletteB,
    /// Engine A extended sprite palette, 8 KiB.
    ObjExtPaletteA,
    /// Engine B extended sprite palette, 8 KiB.
    ObjExtPaletteB,
}

const REGIONS: [Region; 12] = [
    Region::Lcdc,
    Region::BgA,
    Region::ObjA,
    Region::BgB,
    Region::ObjB,
    Region::Arm7,
    Region::Texture,
    Region::TexturePalette,
    Region::BgExtPaletteA,
    Region::BgExtPaletteB,
    Region::ObjExtPaletteA,
    Region::ObjExtPaletteB,
];

impl Region {
    /// Size in bytes; CPU addresses mirror within it.
    pub(crate) const fn size(self) -> u32 {
        match self {
            Self::Lcdc => 0xA_4000,
            Self::BgA | Self::Texture => 0x8_0000,
            Self::ObjA | Self::Arm7 => 0x4_0000,
            Self::BgB | Self::ObjB => 0x2_0000,
            Self::TexturePalette => 0x1_8000,
            Self::BgExtPaletteA | Self::BgExtPaletteB => 0x8000,
            Self::ObjExtPaletteA | Self::ObjExtPaletteB => 0x2000,
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// One of the two 2D graphics engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Engine {
    /// The main engine: 3D, display capture, 512 KiB of BG VRAM.
    A,
    /// The sub engine.
    B,
}

/// What a 2D engine reads from VRAM, by offset within its own memory.
/// Offsets wrap at the region's size. Unmapped memory reads zero.
pub(crate) trait EngineMemory {
    /// The halfword at `offset` in the engine's background VRAM.
    fn bg(&self, offset: u32) -> u16;
    /// The `N` bytes at `offset` (aligned down to `N`, a power of two up to
    /// 8 KiB) in the engine's background VRAM: what `N / 2` calls of
    /// [`EngineMemory::bg`] would read, in one lookup.
    fn bg_bytes<const N: usize>(&self, offset: u32) -> [u8; N];
    /// The halfword at `offset` in the engine's sprite VRAM.
    fn obj(&self, offset: u32) -> u16;
    /// Colour `index` (0–4095) of extended background palette `slot` (0–3).
    fn bg_ext_palette(&self, slot: u32, index: u32) -> u16;
    /// Colour `index` (0–4095) of the extended sprite palette.
    fn obj_ext_palette(&self, index: u32) -> u16;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Vram {
    #[serde(with = "crate::bytes::array")]
    banks: [Box<[u8]>; 9],
    /// VRAMCNT_A to VRAMCNT_I.
    control: [u8; 9],
    /// Where each bank is mapped: region and byte offset within it.
    mapped: [Option<(Region, u32)>; 9],
    /// Per region, per page: the banks covering it (bit n for bank n).
    pages: Vec<Vec<u16>>,
}

impl Default for Vram {
    fn default() -> Self {
        let mut vram = Self {
            banks: BANK_SIZES.map(|size| vec![0; size as usize].into_boxed_slice()),
            control: [0; 9],
            mapped: [None; 9],
            pages: Vec::new(),
        };
        vram.rebuild();
        vram
    }
}

/// Where VRAMCNT value `control` puts `bank` (0 = A … 8 = I), if anywhere.
fn mapping(bank: usize, control: u8) -> Option<(Region, u32)> {
    if control & 0x80 == 0 {
        return None;
    }
    let mst = control & 7;
    let ofs = u32::from((control >> 3) & 3);
    let small = ofs & 1;
    let high = ofs >> 1;
    Some(match (bank, mst) {
        (_, 0) => (Region::Lcdc, LCDC_OFFSETS[bank]),
        (0..=3, 1) => (Region::BgA, ofs * 0x2_0000),
        (0 | 1, 2) => (Region::ObjA, small * 0x2_0000),
        (0..=3, 3) => (Region::Texture, ofs * 0x2_0000),
        (2 | 3, 2) => (Region::Arm7, small * 0x2_0000),
        (2, 4) => (Region::BgB, 0),
        (3, 4) => (Region::ObjB, 0),
        (4, 1) => (Region::BgA, 0),
        (4, 2) => (Region::ObjA, 0),
        (4, 3) => (Region::TexturePalette, 0),
        (4, 4) => (Region::BgExtPaletteA, 0),
        (5 | 6, 1) => (Region::BgA, small * 0x4000 + high * 0x1_0000),
        (5 | 6, 2) => (Region::ObjA, small * 0x4000 + high * 0x1_0000),
        (5 | 6, 3) => (Region::TexturePalette, (small + high * 4) * 0x4000),
        (5 | 6, 4) => (Region::BgExtPaletteA, small * 0x4000),
        (5 | 6, 5) => (Region::ObjExtPaletteA, 0),
        (7, 1) => (Region::BgB, 0),
        (7, 2) => (Region::BgExtPaletteB, 0),
        (8, 1) => (Region::BgB, 0x8000),
        (8, 2) => (Region::ObjB, 0),
        (8, 3) => (Region::ObjExtPaletteB, 0),
        _ => return None,
    })
}

/// `offset` wrapped to `region`'s size; most sizes are powers of two.
fn wrap(region: Region, offset: u32) -> u32 {
    let size = region.size();
    if size.is_power_of_two() {
        offset & (size - 1)
    } else {
        offset % size
    }
}

impl Vram {
    /// VRAMCNT for `bank` (0 = A … 8 = I).
    pub(crate) fn control(&self, bank: usize) -> u8 {
        self.control[bank]
    }

    /// The banks covering the 8 KiB page that holds `offset` in `region`
    /// (wrapped to its size): bit n for bank n (0 = A … 8 = I).
    pub(crate) fn banks_at(&self, region: Region, offset: u32) -> u16 {
        self.pages[region.index()][(wrap(region, offset) / PAGE) as usize]
    }

    /// Writes VRAMCNT for `bank` and remaps.
    pub(crate) fn set_control(&mut self, bank: usize, value: u8) {
        self.control[bank] = value & 0x9F;
        self.mapped[bank] = mapping(bank, value);
        self.rebuild();
    }

    /// VRAMSTAT (ARM7 0x04000240): bit 0 if bank C, bit 1 if bank D is
    /// mapped to the ARM7.
    pub(crate) fn arm7_status(&self) -> u8 {
        let arm7 = |bank: usize| matches!(self.mapped[bank], Some((Region::Arm7, _)));
        u8::from(arm7(2)) | u8::from(arm7(3)) << 1
    }

    fn rebuild(&mut self) {
        self.pages = REGIONS
            .iter()
            .map(|region| vec![0; region.size().div_ceil(PAGE) as usize])
            .collect();
        for (bank, mapped) in self.mapped.iter().enumerate() {
            let Some((region, start)) = *mapped else {
                continue;
            };
            let end = (start + BANK_SIZES[bank]).min(region.size());
            let pages = &mut self.pages[region.index()];
            for page in (start / PAGE)..end.div_ceil(PAGE) {
                pages[page as usize] |= 1 << bank;
            }
        }
    }

    /// The byte at `offset` in `region` (wrapped to its size), ORed over
    /// every bank mapped there.
    pub(crate) fn read_8(&self, region: Region, offset: u32) -> u8 {
        let offset = wrap(region, offset);
        let mut mask = self.pages[region.index()][(offset / PAGE) as usize];
        let mut value = 0;
        while mask != 0 {
            let bank = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if let Some((_, start)) = self.mapped[bank] {
                value |= self.banks[bank][(offset - start) as usize];
            }
        }
        value
    }

    /// The halfword at `offset` (aligned down) in `region`. Renderers call
    /// this per pixel, so the common case of one bank per page reads it
    /// directly.
    pub(crate) fn read_16(&self, region: Region, offset: u32) -> u16 {
        let offset = wrap(region, offset & !1);
        let mask = self.pages[region.index()][(offset / PAGE) as usize];
        if mask.is_power_of_two() {
            let bank = mask.trailing_zeros() as usize;
            if let Some((_, start)) = self.mapped[bank] {
                let index = (offset - start) as usize;
                let bytes = &self.banks[bank][index..index + 2];
                return u16::from_le_bytes([bytes[0], bytes[1]]);
            }
        }
        if mask == 0 {
            return 0;
        }
        u16::from_le_bytes([self.read_8(region, offset), self.read_8(region, offset + 1)])
    }

    /// The `N` bytes at `offset` (aligned down to `N`, a power of two no
    /// larger than a page) in `region`, ORed over every bank mapped there:
    /// a tile row or a row of map entries in one lookup.
    pub(crate) fn read_bytes<const N: usize>(&self, region: Region, offset: u32) -> [u8; N] {
        const { assert!(N.is_power_of_two() && N <= PAGE as usize) };
        let offset = wrap(region, offset & !(N as u32 - 1));
        let mask = self.pages[region.index()][(offset / PAGE) as usize];
        let mut bytes = [0; N];
        if mask.is_power_of_two() {
            let bank = mask.trailing_zeros() as usize;
            if let Some((_, start)) = self.mapped[bank] {
                let index = (offset - start) as usize;
                bytes.copy_from_slice(&self.banks[bank][index..index + N]);
                return bytes;
            }
        }
        for (byte, value) in (offset..).zip(bytes.iter_mut()) {
            *value = self.read_8(region, byte);
        }
        bytes
    }

    /// Copies `out.len()` bytes from `offset` in `region`, which must lie
    /// within one 8 KiB page. Pages one bank covers copy in one go.
    pub(crate) fn copy_page(&self, region: Region, offset: u32, out: &mut [u8]) {
        let offset = wrap(region, offset);
        let mask = self.pages[region.index()][(offset / PAGE) as usize];
        if mask.is_power_of_two() {
            let bank = mask.trailing_zeros() as usize;
            if let Some((_, start)) = self.mapped[bank] {
                let index = (offset - start) as usize;
                out.copy_from_slice(&self.banks[bank][index..index + out.len()]);
                return;
            }
        }
        for (byte, value) in (offset..).zip(out.iter_mut()) {
            *value = self.read_8(region, byte);
        }
    }

    /// Writes `bytes` at `offset` in `region`, which must lie within one
    /// 8 KiB page, to every bank mapped there.
    #[inline]
    pub(crate) fn write_page(&mut self, region: Region, offset: u32, bytes: &[u8]) {
        let offset = wrap(region, offset);
        let mut mask = self.pages[region.index()][(offset / PAGE) as usize];
        while mask != 0 {
            let bank = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if let Some((_, start)) = self.mapped[bank] {
                let index = (offset - start) as usize;
                self.banks[bank][index..index + bytes.len()].copy_from_slice(bytes);
            }
        }
    }

    /// Writes the byte at `offset` in `region` to every bank mapped there.
    pub(crate) fn write_8(&mut self, region: Region, offset: u32, value: u8) {
        self.write_page(region, offset, &[value]);
    }

    /// Writes the halfword at `offset` (aligned down) in `region`.
    pub(crate) fn write_16(&mut self, region: Region, offset: u32, value: u16) {
        self.write_page(region, offset & !1, &value.to_le_bytes());
    }

    /// The region and offset behind ARM9 address `address` in
    /// 0x06000000–0x06FFFFFF, or `None` past the LCDC area.
    pub(crate) fn arm9_region(address: u32) -> Option<(Region, u32)> {
        let offset = address & 0x1F_FFFF;
        match (address >> 21) & 7 {
            0 => Some((Region::BgA, offset)),
            1 => Some((Region::BgB, offset)),
            2 => Some((Region::ObjA, offset)),
            3 => Some((Region::ObjB, offset)),
            _ => {
                let offset = address & 0xF_FFFF;
                (offset < Region::Lcdc.size()).then_some((Region::Lcdc, offset))
            }
        }
    }

    /// A 2D engine's view of VRAM.
    pub(crate) fn engine(&self, engine: Engine) -> EngineView<'_> {
        EngineView { vram: self, engine }
    }
}

/// One 2D engine's VRAM: see [`EngineMemory`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct EngineView<'a> {
    vram: &'a Vram,
    engine: Engine,
}

impl EngineView<'_> {
    fn bg_region(&self) -> Region {
        match self.engine {
            Engine::A => Region::BgA,
            Engine::B => Region::BgB,
        }
    }
}

impl EngineMemory for EngineView<'_> {
    fn bg(&self, offset: u32) -> u16 {
        self.vram.read_16(self.bg_region(), offset)
    }

    fn bg_bytes<const N: usize>(&self, offset: u32) -> [u8; N] {
        self.vram.read_bytes(self.bg_region(), offset)
    }

    fn obj(&self, offset: u32) -> u16 {
        let region = match self.engine {
            Engine::A => Region::ObjA,
            Engine::B => Region::ObjB,
        };
        self.vram.read_16(region, offset)
    }

    fn bg_ext_palette(&self, slot: u32, index: u32) -> u16 {
        let region = match self.engine {
            Engine::A => Region::BgExtPaletteA,
            Engine::B => Region::BgExtPaletteB,
        };
        self.vram
            .read_16(region, (slot & 3) * 0x2000 + (index & 0xFFF) * 2)
    }

    fn obj_ext_palette(&self, index: u32) -> u16 {
        let region = match self.engine {
            Engine::A => Region::ObjExtPaletteA,
            Engine::B => Region::ObjExtPaletteB,
        };
        self.vram.read_16(region, (index & 0xFFF) * 2)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const ENABLE: u8 = 0x80;

    /// A small xorshift generator, so random tests repeat.
    pub(crate) struct Random(pub(crate) u64);

    impl Random {
        pub(crate) fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 32) as u32
        }
    }

    /// The banks and byte indices behind `offset` (wrapped) in `region`
    /// when the VRAMCNT registers hold `controls`, found from each bank's
    /// mapping alone, without the page table.
    fn covering(controls: &[u8; 9], region: Region, offset: u32) -> Vec<(usize, usize)> {
        let offset = offset % region.size();
        (0..9)
            .filter_map(|bank| match mapping(bank, controls[bank]) {
                Some((mapped, start))
                    if mapped == region && (start..start + BANK_SIZES[bank]).contains(&offset) =>
                {
                    Some((bank, (offset - start) as usize))
                }
                _ => None,
            })
            .collect()
    }

    /// A random VRAMCNT value, enabled and mapped to a BG area with OFS 0
    /// or 1 half the time, so that banks often overlap.
    fn random_control(random: &mut Random) -> u8 {
        let value = random.next() as u8;
        if value & 1 == 0 {
            ENABLE | 1 | (value >> 1 & 1) << 3
        } else {
            value
        }
    }

    #[test]
    fn reads_and_writes_match_a_plain_model_of_the_mapping() {
        let mut random = Random(0x0BAC);
        for _ in 0..300 {
            let mut vram = Vram::default();
            let mut banks = BANK_SIZES.map(|size| vec![0u8; size as usize]);
            let mut controls = [0; 9];
            for (bank, control) in controls.iter_mut().enumerate() {
                let value = random_control(&mut random);
                *control = value & 0x9F;
                vram.set_control(bank, value);
            }
            let read = |banks: &[Vec<u8>; 9], controls: &[u8; 9], region, offset| {
                covering(controls, region, offset)
                    .iter()
                    .fold(0, |value, &(bank, index)| value | banks[bank][index])
            };
            for _ in 0..400 {
                // Mostly somewhere a bank is mapped; otherwise anywhere,
                // past the region's size too, as CPU addresses mirror.
                let bank = (random.next() % 9) as usize;
                let (region, offset) = match mapping(bank, controls[bank]) {
                    Some((region, start)) if random.next() & 3 != 0 => {
                        (region, start + random.next() % BANK_SIZES[bank])
                    }
                    _ => {
                        let region = REGIONS[(random.next() % 12) as usize];
                        (region, random.next() % (region.size() * 2))
                    }
                };
                let value = random.next();
                match random.next() % 8 {
                    0 => {
                        let value = random_control(&mut random);
                        controls[bank] = value & 0x9F;
                        vram.set_control(bank, value);
                    }
                    1 => {
                        vram.write_8(region, offset, value as u8);
                        for (bank, index) in covering(&controls, region, offset) {
                            banks[bank][index] = value as u8;
                        }
                    }
                    2 | 3 => {
                        let offset = offset & !1;
                        vram.write_16(region, offset, value as u16);
                        for (byte, value) in (offset..).zip((value as u16).to_le_bytes()) {
                            for (bank, index) in covering(&controls, region, byte) {
                                banks[bank][index] = value;
                            }
                        }
                    }
                    4 => {
                        let offset = offset & !63;
                        let bytes = [0; 64].map(|_| random.next() as u8);
                        vram.write_page(region, offset, &bytes);
                        for (byte, &value) in (offset..).zip(&bytes) {
                            for (bank, index) in covering(&controls, region, byte) {
                                banks[bank][index] = value;
                            }
                        }
                    }
                    5 => {
                        let expected = read(&banks, &controls, region, offset);
                        assert_eq!(vram.read_8(region, offset), expected);
                    }
                    6 => {
                        let offset = offset & !1;
                        let expected = [0, 1].map(|n| read(&banks, &controls, region, offset + n));
                        let expected = u16::from_le_bytes(expected);
                        assert_eq!(
                            vram.read_16(region, offset),
                            expected,
                            "{region:?} {offset:#X}"
                        );
                    }
                    _ => {
                        let offset = offset & !7;
                        let expected: Vec<u8> = (offset..offset + 8)
                            .map(|byte| read(&banks, &controls, region, byte))
                            .collect();
                        let bytes: [u8; 8] = vram.read_bytes(region, offset);
                        assert_eq!(bytes[..], expected[..], "{region:?} {offset:#X}");
                    }
                }
            }
        }
    }

    #[test]
    fn lcdc_mapping_shows_each_bank_at_its_fixed_place() {
        let mut vram = Vram::default();
        vram.set_control(0, ENABLE); // A to LCDC
        vram.write_16(Region::Lcdc, 0x10, 0xBEEF);
        assert_eq!(vram.read_16(Region::Lcdc, 0x10), 0xBEEF);
        assert_eq!(vram.read_16(Region::Lcdc, 0x2_0010), 0, "B is not mapped");
        assert_eq!(Vram::arm9_region(0x0680_0010), Some((Region::Lcdc, 0x10)));
    }

    #[test]
    fn remapping_moves_the_data() {
        let mut vram = Vram::default();
        vram.set_control(0, ENABLE);
        vram.write_16(Region::Lcdc, 0x100, 0x1234);
        vram.set_control(0, ENABLE | 1 | 2 << 3); // A to engine A BG, offset 2
        assert_eq!(vram.engine(Engine::A).bg(0x4_0100), 0x1234);
        assert_eq!(vram.read_16(Region::Lcdc, 0x100), 0);
        // CPU addresses mirror the 512 KiB BG region.
        assert_eq!(
            Vram::arm9_region(0x0608_0100),
            Some((Region::BgA, 0x8_0100))
        );
        assert_eq!(vram.read_16(Region::BgA, 0x8_0100 + 0x4_0000), 0x1234);
    }

    #[test]
    fn overlapping_banks_or_reads_and_share_writes() {
        let mut vram = Vram::default();
        vram.set_control(5, ENABLE); // F to LCDC, to fill it
        vram.write_8(Region::Lcdc, 0x9_0000, 0x0F);
        vram.set_control(6, ENABLE); // G to LCDC
        vram.write_8(Region::Lcdc, 0x9_4000, 0xF0);
        vram.set_control(5, ENABLE | 1); // F to engine A BG at 0
        vram.set_control(6, ENABLE | 1); // G there too
        assert_eq!(vram.read_8(Region::BgA, 0), 0xFF);
        vram.write_8(Region::BgA, 1, 0x42);
        vram.set_control(6, ENABLE);
        assert_eq!(vram.read_8(Region::Lcdc, 0x9_4001), 0x42);
    }

    #[test]
    fn small_banks_map_by_offset() {
        let mut vram = Vram::default();
        // G to engine A BG with OFS 3: 0x4000 + 0x10000.
        vram.set_control(6, ENABLE | 1 | 3 << 3);
        vram.write_16(Region::BgA, 0x1_4000, 7);
        assert_eq!(vram.engine(Engine::A).bg(0x1_4000), 7);
        assert_eq!(vram.engine(Engine::A).bg(0x1_0000), 0);
    }

    #[test]
    fn page_copies_match_byte_reads() {
        let mut vram = Vram::default();
        vram.set_control(0, ENABLE | 3); // A to texture slot 0
        vram.write_16(Region::Texture, 0x2002, 0xBEEF);
        let mut page = [0; PAGE as usize];
        vram.copy_page(Region::Texture, 0x2000, &mut page);
        assert_eq!(&page[2..4], &[0xEF, 0xBE]);
        vram.copy_page(Region::Texture, 0x2_2000, &mut page);
        assert!(page.iter().all(|byte| *byte == 0), "slot 1 is unmapped");
    }

    #[test]
    fn arm7_status_reports_c_and_d() {
        let mut vram = Vram::default();
        vram.set_control(2, ENABLE | 2); // C to ARM7 slot 0
        vram.set_control(3, ENABLE | 2 | 1 << 3); // D to ARM7 slot 1
        assert_eq!(vram.arm7_status(), 0b11);
        vram.write_16(Region::Arm7, 0x2_0000, 9);
        vram.set_control(3, ENABLE);
        assert_eq!(vram.arm7_status(), 0b01);
        assert_eq!(vram.read_16(Region::Lcdc, 0x6_0000), 9);
    }

    #[test]
    fn extended_palettes_index_by_slot() {
        let mut vram = Vram::default();
        vram.set_control(4, ENABLE | 4); // E to engine A BG extended palettes
        vram.write_16(Region::BgExtPaletteA, 3 * 0x2000 + 5 * 2, 0x7FFF);
        assert_eq!(vram.engine(Engine::A).bg_ext_palette(3, 5), 0x7FFF);
        vram.set_control(8, ENABLE | 3); // I to engine B OBJ extended palette
        vram.write_16(Region::ObjExtPaletteB, 2, 0x001F);
        assert_eq!(vram.engine(Engine::B).obj_ext_palette(1), 0x001F);
    }
}
