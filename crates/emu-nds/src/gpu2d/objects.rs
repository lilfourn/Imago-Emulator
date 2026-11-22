//! Sprites (GBATEK, "DS Video OBJs", "DS Video Extended Palettes", and the
//! GBA's "LCD OBJ - OAM Attributes", "LCD OBJ - OAM Rotation/Scaling
//! Parameters" and "LCD OBJ - VRAM Character (Tile) Mapping").
//!
//! All 128 sprites draw into one line buffer that keeps, per pixel, the
//! front-most sprite: lowest priority number, then lowest OAM index (the
//! DS's 9-bit OBJ priority). OBJ-window sprites only mark the window mask.

use super::cache::HalfwordCache;
use super::{Engine2d, WIDTH, layer_color};
use crate::vram::EngineMemory;

/// Sprite sizes by shape (square, wide, tall) and size field.
const OBJ_SIZES: [[(u32, u32); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];

/// How a sprite pixel takes part in blending.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ObjBlend {
    #[default]
    Normal,
    /// OBJ mode 1: blends over a 2nd target with BLDALPHA's EVA and EVB.
    SemiTransparent,
    /// A bitmap sprite: blends over a 2nd target with its Alpha-OAM value
    /// (attribute 2 bits 12–15, 1–15).
    Bitmap(u8),
}

/// The front-most sprite at one pixel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ObjPixel {
    /// Layer pixel, 0 where no sprite is drawn.
    pub(super) color: u32,
    /// Priority against backgrounds, 0 (front) to 3.
    pub(super) priority: u8,
    pub(super) blend: ObjBlend,
}

/// How a sprite's texels are stored.
#[derive(Debug, Clone, Copy)]
enum Texels {
    /// 16-colour tiles using standard OBJ palette `palette` (0–15).
    Tiles16 { palette: u32 },
    /// 256-colour tiles; `ext_base` is the first colour of the sprite's
    /// extended palette when DISPCNT bit 31 enables extended OBJ palettes.
    Tiles256 { ext_base: Option<u32> },
    /// Direct colour with bit 15 as alpha (clear is transparent).
    Bitmap,
}

/// Where a sprite's texels are in OBJ VRAM.
struct Layout {
    texels: Texels,
    /// Byte address of the top-left texel.
    base: u32,
    /// Bytes from one row of 8×8 tiles to the next, or from one bitmap row
    /// to the next.
    stride: u32,
    /// Address mask: 2D tile mapping wraps within 32 KiB.
    mask: u32,
}

impl Engine2d {
    /// Draws `line` of every sprite into `pixels`, and marks the opaque
    /// pixels of OBJ-window sprites in `window`.
    pub(super) fn draw_objects(
        &self,
        line: u32,
        memory: &impl EngineMemory,
        pixels: &mut [ObjPixel; WIDTH],
        window: &mut [bool; WIDTH],
    ) {
        let (mosaic_x, mosaic_y) = self.registers.obj_mosaic();
        for attributes in self.oam.chunks_exact(4) {
            let (attr0, attr1, attr2) = (attributes[0], attributes[1], attributes[2]);
            let affine = attr0 & (1 << 8) != 0;
            if !affine && attr0 & (1 << 9) != 0 {
                continue; // hidden
            }
            let shape = usize::from(attr0 >> 14);
            if shape == 3 {
                continue;
            }
            let (width, height) = OBJ_SIZES[shape][usize::from(attr1 >> 14)];
            let double = affine && attr0 & (1 << 9) != 0;
            let (box_width, box_height) = if double {
                (width * 2, height * 2)
            } else {
                (width, height)
            };
            // Rows count within 256 lines, so unlike the GBA a sprite
            // crossing the bottom edge shows at the top as well.
            let top = u32::from(attr0 & 0xFF);
            let row_at = |line: u32| line.wrapping_sub(top) & 0xFF;
            if row_at(line) >= box_height {
                continue;
            }
            let mosaic = attr0 & (1 << 12) != 0;
            let row = if mosaic {
                row_at(line - line % mosaic_y)
            } else {
                row_at(line)
            };
            if row >= box_height {
                continue; // the mosaic block starts above the sprite
            }

            let mode = (attr0 >> 10) & 3;
            let blend = match mode {
                1 => ObjBlend::SemiTransparent,
                3 => ObjBlend::Bitmap((attr2 >> 12) as u8),
                _ => ObjBlend::Normal,
            };
            if blend == ObjBlend::Bitmap(0) {
                continue; // Alpha-OAM 0 hides a bitmap sprite
            }
            let Some(layout) = self.layout(attr0, attr2, width) else {
                continue;
            };
            let priority = ((attr2 >> 10) & 3) as u8;
            let (pa, pb, pc, pd) = if affine {
                let group = usize::from((attr1 >> 9) & 0x1F) * 16;
                let parameter = |n: usize| i32::from(self.oam[group + 3 + n * 4] as i16);
                (parameter(0), parameter(1), parameter(2), parameter(3))
            } else {
                (0x100, 0, 0, 0x100)
            };
            let h_flip = !affine && attr1 & (1 << 12) != 0;
            let v_flip = !affine && attr1 & (1 << 13) != 0;
            let left = i32::from(attr1 & 0x1FF) - if attr1 & 0x100 != 0 { 512 } else { 0 };
            // Texture coordinates are relative to the sprite's centre.
            let dy = row as i32 - box_height as i32 / 2;
            let (half_width, half_height) = (width as i32 / 2, height as i32 / 2);

            let mut cache = HalfwordCache::new();
            let visible = (-left).max(0)..(WIDTH as i32 - left).min(box_width as i32);
            for box_x in visible {
                let screen_x = (left + box_x) as usize;
                let sample_x = if mosaic {
                    box_x - (screen_x as u32 % mosaic_x) as i32
                } else {
                    box_x
                };
                let dx = sample_x - box_width as i32 / 2;
                let tx = ((pa * dx + pb * dy) >> 8) + half_width;
                let ty = ((pc * dx + pd * dy) >> 8) + half_height;
                if !(0..width as i32).contains(&tx) || !(0..height as i32).contains(&ty) {
                    continue;
                }
                let (mut tx, mut ty) = (tx as u32, ty as u32);
                if h_flip {
                    tx = width - 1 - tx;
                }
                if v_flip {
                    ty = height - 1 - ty;
                }
                let color = self.texel(memory, &layout, &mut cache, tx, ty);
                if color == 0 {
                    continue;
                }
                if mode == 2 {
                    window[screen_x] = true;
                    continue;
                }
                let current = &mut pixels[screen_x];
                if current.color == 0 || priority < current.priority {
                    *current = ObjPixel {
                        color,
                        priority,
                        blend,
                    };
                }
            }
        }
    }

    /// Where a `width`-pixel-wide sprite's texels are, from its attributes
    /// and DISPCNT's mapping bits; `None` for the reserved bitmap mapping.
    fn layout(&self, attr0: u16, attr2: u16, width: u32) -> Option<Layout> {
        let dispcnt = self.registers.dispcnt;
        let tile = u32::from(attr2 & 0x3FF);
        if (attr0 >> 10) & 3 == 3 {
            let (base, stride) = match (dispcnt >> 5) & 3 {
                // 2D: one 128- or 256-dot-wide bitmap. The tile number's
                // low 4 or 5 bits pick an 8-dot column, the rest an 8-dot
                // row.
                dimension @ (0 | 1) => {
                    let mask = if dimension == 0 { 0xF } else { 0x1F };
                    let base = (tile & mask) * 0x10 + (tile & !mask) * 0x80;
                    (base, (mask + 1) * 16)
                }
                // 1D: the tile number counts 128- or 256-byte units (bit
                // 22 is engine A's only).
                2 => (tile * (128 << ((dispcnt >> 22) & 1)), width * 2),
                _ => return None,
            };
            return Some(Layout {
                texels: Texels::Bitmap,
                base,
                stride,
                mask: u32::MAX,
            });
        }
        let eight_bit = attr0 & (1 << 13) != 0;
        let palette = u32::from(attr2 >> 12);
        let texels = if !eight_bit {
            Texels::Tiles16 { palette }
        } else {
            Texels::Tiles256 {
                ext_base: (dispcnt & (1 << 31) != 0).then_some(palette * 256),
            }
        };
        Some(if dispcnt & (1 << 4) != 0 {
            // 1D: a sprite's tiles follow each other, from the tile number
            // in units of the boundary (DISPCNT bits 20–21: 32–256 bytes).
            let tile_bytes = if eight_bit { 64 } else { 32 };
            Layout {
                texels,
                base: tile * (32 << ((dispcnt >> 20) & 3)),
                stride: width / 8 * tile_bytes,
                mask: u32::MAX,
            }
        } else {
            // 2D: a sheet 32 tiles wide in the first 32 KiB.
            Layout {
                texels,
                base: tile * 32,
                stride: 32 * 32,
                mask: 0x7FFF,
            }
        })
    }

    /// The layer pixel at texel (`x`, `y`) of a sprite, 0 if transparent.
    fn texel(
        &self,
        memory: &impl EngineMemory,
        layout: &Layout,
        cache: &mut HalfwordCache,
        x: u32,
        y: u32,
    ) -> u32 {
        let read = |offset| memory.obj(offset);
        match layout.texels {
            Texels::Bitmap => {
                let color = cache.halfword(layout.base + y * layout.stride + x * 2, read);
                if color & 0x8000 != 0 {
                    layer_color(color)
                } else {
                    0
                }
            }
            Texels::Tiles16 { palette } => {
                let tile = layout.base + y / 8 * layout.stride + x / 8 * 32;
                let address = (tile + y % 8 * 4 + x % 8 / 2) & layout.mask;
                let index = (cache.byte(address, read) >> (x % 2 * 4)) & 0xF;
                if index == 0 {
                    0
                } else {
                    self.obj_color(palette * 16 + u32::from(index))
                }
            }
            Texels::Tiles256 { ext_base } => {
                let tile = layout.base + y / 8 * layout.stride + x / 8 * 64;
                let index = cache.byte((tile + y % 8 * 8 + x % 8) & layout.mask, read);
                match ext_base {
                    _ if index == 0 => 0,
                    Some(base) => layer_color(memory.obj_ext_palette(base + u32::from(index))),
                    None => self.obj_color(u32::from(index)),
                }
            }
        }
    }
}
