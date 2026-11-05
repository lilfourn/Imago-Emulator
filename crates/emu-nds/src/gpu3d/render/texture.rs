//! Texture memory and texel fetch for the seven texture formats (GBATEK,
//! "DS 3D Texture Attributes" and "DS 3D Texture Formats").
//!
//! The renderer copies texture and texture-palette VRAM in 8 KiB pages the
//! first time a polygon in the frame needs them, then reads texels from
//! the copy without going through bank mapping per texel.

use crate::gpu3d::color;
use crate::gpu3d::polygon::TextureParam;
use crate::vram::{Region, Vram};

const TEXTURE_SIZE: u32 = 0x8_0000;
const PALETTE_SIZE: u32 = 0x1_8000;
const PAGE: u32 = 0x2000;

/// This frame's copy of the texture and texture-palette regions.
#[derive(Debug, Clone)]
pub(crate) struct TextureMemory {
    texture: Box<[u8]>,
    palette: Box<[u8]>,
    /// One bit per page copied since [`TextureMemory::invalidate`].
    texture_pages: u64,
    palette_pages: u64,
}

impl Default for TextureMemory {
    fn default() -> Self {
        Self {
            texture: vec![0; TEXTURE_SIZE as usize].into_boxed_slice(),
            palette: vec![0; PALETTE_SIZE as usize].into_boxed_slice(),
            texture_pages: 0,
            palette_pages: 0,
        }
    }
}

/// Copies the pages of `region` covering `len` bytes from `start`
/// (wrapping) that are not copied yet.
fn load(vram: &Vram, region: Region, buffer: &mut [u8], pages: &mut u64, start: u32, len: u32) {
    let size = buffer.len() as u32;
    let page_count = size / PAGE;
    let first = (start % size) / PAGE;
    let count = len.min(size).div_ceil(PAGE) + 1;
    for page in (first..first + count).map(|page| page % page_count) {
        if *pages & (1 << page) != 0 {
            continue;
        }
        *pages |= 1 << page;
        let base = page * PAGE;
        vram.copy_page(
            region,
            base,
            &mut buffer[base as usize..(base + PAGE) as usize],
        );
    }
}

impl TextureMemory {
    /// Forgets the copies, so the next frame sees VRAM afresh.
    pub(crate) fn invalidate(&mut self) {
        self.texture_pages = 0;
        self.palette_pages = 0;
    }

    pub(crate) fn load_texture(&mut self, vram: &Vram, start: u32, len: u32) {
        load(
            vram,
            Region::Texture,
            &mut self.texture,
            &mut self.texture_pages,
            start,
            len,
        );
    }

    pub(crate) fn load_palette(&mut self, vram: &Vram, start: u32, len: u32) {
        load(
            vram,
            Region::TexturePalette,
            &mut self.palette,
            &mut self.palette_pages,
            start,
            len,
        );
    }

    pub(crate) fn texture_8(&self, address: u32) -> u8 {
        self.texture[(address % TEXTURE_SIZE) as usize]
    }

    pub(crate) fn texture_16(&self, address: u32) -> u16 {
        u16::from_le_bytes([self.texture_8(address), self.texture_8(address + 1)])
    }

    fn texture_32(&self, address: u32) -> u32 {
        u32::from(self.texture_16(address)) | u32::from(self.texture_16(address + 2)) << 16
    }

    fn palette_16(&self, address: u32) -> u16 {
        let byte = |address: u32| self.palette[(address % PALETTE_SIZE) as usize];
        u16::from_le_bytes([byte(address), byte(address + 1)])
    }
}

/// A texel: 6-bit colour and 5-bit alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Texel {
    pub(crate) rgb: [u8; 3],
    pub(crate) alpha: u8,
}

const TRANSPARENT: Texel = Texel {
    rgb: [0; 3],
    alpha: 0,
};

fn opaque(bgr555: u16) -> Texel {
    Texel {
        rgb: color::rgb6(u32::from(bgr555)),
        alpha: 31,
    }
}

/// `(a * a_weight + b * b_weight) / 8` per BGR555 channel; weights sum to 8.
fn mix(a: u16, b: u16, a_weight: u16, b_weight: u16) -> u16 {
    (0..3).fold(0, |out, channel| {
        let shift = channel * 5;
        let mixed = (((a >> shift) & 0x1F) * a_weight + ((b >> shift) & 0x1F) * b_weight) / 8;
        out | mixed << shift
    })
}

/// A texture coordinate in texels, wrapped, mirrored or clamped into
/// `0..1 << shift`.
fn wrap(coord: i32, shift: u32, repeat: bool, flip: bool) -> u32 {
    let size = 1 << shift;
    if !repeat {
        return coord.clamp(0, size - 1) as u32;
    }
    let wrapped = coord & (size - 1);
    if flip && coord & size != 0 {
        (size - 1 - wrapped) as u32
    } else {
        wrapped as u32
    }
}

/// Where a 4×4 block's palette index data lives in slot 1, for the block
/// at `address` in slot 0 or 2.
fn block_info_address(address: u32) -> u32 {
    let address = address % TEXTURE_SIZE;
    let upper_half = if address >= 0x4_0000 { 0x1_0000 } else { 0 };
    0x2_0000 + ((address & 0x1_FFFF) >> 1) + upper_half
}

/// Reads texels of one texture.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sampler {
    param: TextureParam,
    /// Palette byte address.
    palette: u32,
}

impl Sampler {
    /// `None` for format 0 (no texture).
    pub(crate) fn new(param: TextureParam, palette_base: u32) -> Option<Self> {
        let palette = if param.format() == 2 {
            palette_base << 3
        } else {
            palette_base << 4
        };
        (param.format() != 0).then_some(Self { param, palette })
    }

    /// Copies whatever VRAM this texture reads into `memory`.
    pub(crate) fn load(&self, memory: &mut TextureMemory, vram: &Vram) {
        let format = self.param.format();
        let texels = 1u32 << (self.param.width_shift() + self.param.height_shift());
        let address = self.param.address();
        let bits_per_texel = match format {
            2 | 5 => 2,
            3 => 4,
            7 => 16,
            _ => 8,
        };
        memory.load_texture(vram, address, texels * bits_per_texel / 8);
        if format == 5 {
            // 16 bits of block info per 16 texels, and palette offsets up
            // to 0x3FFF * 4.
            memory.load_texture(vram, block_info_address(address), texels / 8);
            memory.load_palette(vram, self.palette, 0x1_0008);
            return;
        }
        let colors = match format {
            1 => 32,
            2 => 4,
            3 => 16,
            4 => 256,
            6 => 8,
            _ => return,
        };
        memory.load_palette(vram, self.palette, colors * 2);
    }

    /// The texel at `s`, `t` (in texels, before wrapping). Inlined into
    /// the rasterizer's loop, where it is most of the work per pixel.
    #[inline(always)]
    pub(crate) fn sample(&self, memory: &TextureMemory, s: i32, t: i32) -> Texel {
        let param = self.param;
        let width_shift = param.width_shift();
        let s = wrap(s, width_shift, param.repeat_s(), param.flip_s());
        let t = wrap(t, param.height_shift(), param.repeat_t(), param.flip_t());
        let index = (t << width_shift) + s;
        let address = param.address();
        match param.format() {
            1 => {
                let value = memory.texture_8(address + index);
                let alpha = value >> 5;
                self.translucent(memory, value & 0x1F, alpha * 4 + alpha / 2)
            }
            2 => {
                let byte = memory.texture_8(address + index / 4);
                self.indexed(memory, (byte >> ((index & 3) * 2)) & 3)
            }
            3 => {
                let byte = memory.texture_8(address + index / 2);
                self.indexed(memory, (byte >> ((index & 1) * 4)) & 0xF)
            }
            4 => self.indexed(memory, memory.texture_8(address + index)),
            5 => self.compressed(memory, s, t),
            6 => {
                let value = memory.texture_8(address + index);
                self.translucent(memory, value & 7, value >> 3)
            }
            _ => {
                let value = memory.texture_16(address + index * 2);
                let mut texel = opaque(value);
                texel.alpha = if value & 0x8000 != 0 { 31 } else { 0 };
                texel
            }
        }
    }

    fn color(&self, memory: &TextureMemory, index: u8) -> u16 {
        memory.palette_16(self.palette + u32::from(index) * 2)
    }

    #[inline]
    fn indexed(&self, memory: &TextureMemory, index: u8) -> Texel {
        if index == 0 && self.param.color0_transparent() {
            TRANSPARENT
        } else {
            opaque(self.color(memory, index))
        }
    }

    fn translucent(&self, memory: &TextureMemory, index: u8, alpha: u8) -> Texel {
        let mut texel = opaque(self.color(memory, index));
        texel.alpha = alpha;
        texel
    }

    fn compressed(&self, memory: &TextureMemory, s: u32, t: u32) -> Texel {
        let blocks_per_row = 1 << (self.param.width_shift() - 2);
        let block = (t >> 2) * blocks_per_row + (s >> 2);
        let block_address = self.param.address() + block * 4;
        let texel = (memory.texture_32(block_address) >> ((t & 3) * 8 + (s & 3) * 2)) & 3;
        let info = memory.texture_16(block_info_address(block_address));
        let palette = self.palette + u32::from(info & 0x3FFF) * 4;
        let color = |i: u32| memory.palette_16(palette + i * 2);
        match (info >> 14, texel) {
            (_, 0 | 1) | (0 | 2, 2) | (2, 3) => opaque(color(texel)),
            (1, 2) => opaque(mix(color(0), color(1), 4, 4)),
            (3, 2) => opaque(mix(color(0), color(1), 5, 3)),
            (3, 3) => opaque(mix(color(0), color(1), 3, 5)),
            _ => TRANSPARENT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Format, size shifts (0 = 8 texels), repeat and flip bits.
    fn param(format: u32, size: u32, flags: u32) -> TextureParam {
        TextureParam(format << 26 | size << 23 | size << 20 | flags << 16)
    }

    fn memory_with(texture: &[(u32, u8)], palette: &[(u32, u16)]) -> TextureMemory {
        let mut memory = TextureMemory::default();
        for &(address, value) in texture {
            memory.texture[address as usize] = value;
        }
        for &(address, value) in palette {
            memory.palette[address as usize..address as usize + 2]
                .copy_from_slice(&value.to_le_bytes());
        }
        memory
    }

    #[test]
    fn coordinates_clamp_repeat_and_mirror() {
        assert_eq!(wrap(-3, 3, false, false), 0);
        assert_eq!(wrap(9, 3, false, false), 7);
        assert_eq!(wrap(9, 3, true, false), 1);
        assert_eq!(wrap(9, 3, true, true), 6);
        assert_eq!(wrap(-1, 3, true, true), 0);
    }

    #[test]
    fn four_colour_texels_read_two_bits_each() {
        // Row 0: texels 0..3 are colours 0, 1, 2, 3.
        let memory = memory_with(&[(0, 0b1110_0100)], &[(2, 0x001F), (6, 0x7C00)]);
        let sampler = Sampler::new(param(2, 0, 0), 0).expect("textured");
        assert_eq!(sampler.sample(&memory, 1, 0).rgb, [63, 0, 0]);
        assert_eq!(sampler.sample(&memory, 3, 0).rgb, [0, 0, 63]);
        assert_eq!(sampler.sample(&memory, 0, 0).alpha, 31);
        let transparent =
            Sampler::new(TextureParam(param(2, 0, 0).0 | 1 << 29), 0).expect("textured");
        assert_eq!(transparent.sample(&memory, 0, 0).alpha, 0);
    }

    #[test]
    fn translucent_formats_carry_alpha() {
        let memory = memory_with(&[(0, 7 << 5 | 1), (1, 16 << 3 | 1)], &[(2, 0x7FFF)]);
        let a3i5 = Sampler::new(param(1, 0, 0), 0).expect("textured");
        assert_eq!(
            a3i5.sample(&memory, 0, 0),
            Texel {
                rgb: [63; 3],
                alpha: 31
            }
        );
        let a5i3 = Sampler::new(param(6, 0, 0), 0).expect("textured");
        assert_eq!(a5i3.sample(&memory, 1, 0).alpha, 16);
    }

    #[test]
    fn compressed_blocks_interpolate_their_two_colours() {
        // Block 0 at 0: texel (0,0) = 2, texel (1,0) = 3. Info at 0x20000:
        // mode 3, palette offset 0.
        let memory = memory_with(
            &[(0, 0b1110), (0x2_0001, 0xC0)],
            &[(0, 0x0000), (2, 0x001F)],
        );
        let sampler = Sampler::new(param(5, 0, 0), 0).expect("textured");
        // (0 * 5 + 31 * 3) / 8 = 11; (0 * 3 + 31 * 5) / 8 = 19.
        assert_eq!(sampler.sample(&memory, 0, 0).rgb[0], color::expand(11));
        assert_eq!(sampler.sample(&memory, 1, 0).rgb[0], color::expand(19));
    }

    #[test]
    fn direct_texels_use_bit_15_as_alpha() {
        let memory = memory_with(&[(0, 0x1F), (1, 0x80)], &[]);
        let sampler = Sampler::new(param(7, 0, 0), 0).expect("textured");
        assert_eq!(
            sampler.sample(&memory, 0, 0),
            Texel {
                rgb: [63, 0, 0],
                alpha: 31
            }
        );
        assert_eq!(sampler.sample(&memory, 1, 0).alpha, 0);
    }
}
