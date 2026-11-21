//! Draws one line (GBATEK, "LCD VRAM Overview", "LCD VRAM BG Screen Data
//! Format", "LCD OBJ - OAM Attributes", "LCD I/O Window Feature", "LCD I/O
//! Color Special Effects", "LCD I/O Mosaic Function").
//!
//! Each layer (four backgrounds and the objects) is drawn into its own line
//! buffer of BGR555 colours with transparency; then every pixel picks the
//! top two visible layers by priority, inside whatever window covers it,
//! and blends them.

use super::{HEIGHT, Ppu, WIDTH, color};

/// Background or object pixel: BGR555 colour, or `None` for transparent.
type Layer = [Option<u16>; WIDTH];

/// An object pixel with what composition needs to know.
#[derive(Debug, Clone, Copy, Default)]
struct ObjPixel {
    color: Option<u16>,
    priority: u8,
    semi_transparent: bool,
}

/// Where a text background's map and tiles are, and their format.
struct TextMap {
    screen_base: usize,
    char_base: usize,
    eight_bit: bool,
    /// The map's width in pixels: 256 or 512.
    width: usize,
}

/// Layer numbers in blend-target bit order.
const OBJ: usize = 4;
const BACKDROP: usize = 5;

/// Object sizes by shape (square, wide, tall) and size field.
const OBJ_SIZES: [[(u32, u32); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];

impl Ppu {
    fn reg(&self, offset: usize) -> u16 {
        self.register16(offset)
    }

    /// Draws line `line` into `row` as `0xFFRRGGBB`.
    pub(super) fn draw_line(&self, line: usize, row: &mut [u32; WIDTH]) {
        let dispcnt = self.reg(0x00);
        let mode = dispcnt & 7;
        let mut backgrounds: [Option<Layer>; 4] = [None; 4];
        for (index, layer) in backgrounds.iter_mut().enumerate() {
            if dispcnt & (1 << (8 + index)) == 0 {
                continue;
            }
            *layer = match (mode, index) {
                (0, _) | (1, 0 | 1) => Some(self.text_layer(index, line)),
                (1 | 2, 2) | (2, 3) => Some(self.affine_layer(index, false)),
                (3..=5, 2) => Some(self.affine_layer(index, true)),
                _ => None,
            };
        }
        let (objects, object_window) = if dispcnt & (1 << 12) != 0 {
            self.object_layer(line, dispcnt)
        } else {
            ([ObjPixel::default(); WIDTH], [false; WIDTH])
        };
        self.compose(line, dispcnt, &backgrounds, &objects, &object_window, row);
    }

    // ----- Backgrounds -------------------------------------------------

    fn mosaic(&self) -> (usize, usize, usize, usize) {
        let mosaic = self.reg(0x4C);
        let size = |shift: u16| usize::from((mosaic >> shift) & 0xF) + 1;
        (size(0), size(4), size(8), size(12))
    }

    fn text_layer(&self, index: usize, line: usize) -> Layer {
        let control = self.reg(0x08 + index * 2);
        let scroll_x = usize::from(self.reg(0x10 + index * 4) & 0x1FF);
        let scroll_y = usize::from(self.reg(0x12 + index * 4) & 0x1FF);
        let char_base = usize::from((control >> 2) & 3) * 0x4000;
        let screen_base = usize::from((control >> 8) & 0x1F) * 0x800;
        let eight_bit = control & (1 << 7) != 0;
        let (width, height) = match control >> 14 {
            0 => (256, 256),
            1 => (512, 256),
            2 => (256, 512),
            _ => (512, 512),
        };
        let (mosaic_x, mosaic_y, ..) = self.mosaic();
        let mosaic = control & (1 << 6) != 0;
        let line = if mosaic { line - line % mosaic_y } else { line };
        let y = (line + scroll_y) % height;
        let map = TextMap {
            screen_base,
            char_base,
            eight_bit,
            width,
        };

        // Whole tile rows, from the one under the left edge: a tile more
        // than the screen, starting `skip` pixels left of it.
        let skip = scroll_x % 8;
        let rows: [[Option<u16>; 8]; WIDTH / 8 + 1] = std::array::from_fn(|tile| {
            // The width is a power of two.
            let x = (scroll_x - skip + tile * 8) & (width - 1);
            self.text_tile_row(&map, x / 8, y)
        });
        let mut layer: Layer = rows.as_flattened()[skip..skip + WIDTH]
            .try_into()
            .expect("a tile's width more than the screen");
        if mosaic {
            apply_mosaic(&mut layer, mosaic_x);
        }
        layer
    }

    /// The colours of the eight pixels of line `y` (in map pixels) of the
    /// tile in map column `column`, left to right after flipping, `None`
    /// where transparent.
    fn text_tile_row(&self, map: &TextMap, column: usize, y: usize) -> [Option<u16>; 8] {
        let x = column * 8;
        // Maps wider or taller than 256 are made of 32x32-tile blocks.
        let block = x / 256 + (y / 256) * (map.width / 256);
        let entry_offset =
            map.screen_base + block * 0x800 + ((y % 256) / 8) * 64 + ((x % 256) / 8) * 2;
        let entry = self.vram_16(entry_offset);
        let tile = usize::from(entry & 0x3FF);
        let row = if entry & (1 << 11) != 0 {
            7 - y % 8
        } else {
            y % 8
        };
        // Colour numbers, left to right as stored.
        let mut indices: [u8; 8] = if map.eight_bit {
            self.bg_tile_bytes(map.char_base + tile * 64 + row * 8)
        } else {
            let bytes: [u8; 4] = self.bg_tile_bytes(map.char_base + tile * 32 + row * 4);
            let palette = (entry >> 12) as u8 * 16;
            std::array::from_fn(|pixel| {
                let nibble = (bytes[pixel / 2] >> (pixel % 2 * 4)) & 0xF;
                if nibble == 0 { 0 } else { palette + nibble }
            })
        };
        if entry & (1 << 10) != 0 {
            indices.reverse();
        }
        indices.map(|index| (index != 0).then(|| self.palette_16(usize::from(index) * 2)))
    }

    /// Affine background (modes 1–2) or bitmap (modes 3–5) for BG2/BG3,
    /// sampled through the rotation/scaling matrix.
    fn affine_layer(&self, index: usize, bitmap: bool) -> Layer {
        let control = self.reg(0x08 + index * 2);
        let base = 0x20 + (index - 2) * 0x10;
        let pa = i32::from(self.reg(base) as i16);
        let pc = i32::from(self.reg(base + 4) as i16);
        let (mut x, mut y) = self.affine_reference[index - 2];
        let (mosaic_x, ..) = self.mosaic();
        let mosaic = control & (1 << 6) != 0;
        let mut layer = [None; WIDTH];
        for (screen_x, pixel) in layer.iter_mut().enumerate() {
            if mosaic && screen_x % mosaic_x != 0 {
                // Filled from the start of its mosaic block below.
                x += pa;
                y += pc;
                continue;
            }
            let (tx, ty) = (x >> 8, y >> 8);
            x += pa;
            y += pc;
            *pixel = if bitmap {
                self.bitmap_pixel(tx, ty)
            } else {
                self.affine_tile_pixel(control, tx, ty)
            };
        }
        if mosaic {
            apply_mosaic(&mut layer, mosaic_x);
        }
        layer
    }

    fn affine_tile_pixel(&self, control: u16, tx: i32, ty: i32) -> Option<u16> {
        let size = 128 << (control >> 14);
        let wrap = control & (1 << 13) != 0;
        let (tx, ty) = if wrap {
            (tx.rem_euclid(size), ty.rem_euclid(size))
        } else if (0..size).contains(&tx) && (0..size).contains(&ty) {
            (tx, ty)
        } else {
            return None;
        };
        let char_base = usize::from((control >> 2) & 3) * 0x4000;
        let screen_base = usize::from((control >> 8) & 0x1F) * 0x800;
        let (tx, ty) = (tx as usize, ty as usize);
        let tiles_per_row = size as usize / 8;
        let tile = usize::from(self.bg_vram_byte(screen_base + (ty / 8) * tiles_per_row + tx / 8));
        let index = self.bg_vram_byte(char_base + tile * 64 + (ty % 8) * 8 + tx % 8);
        (index != 0).then(|| self.palette_16(usize::from(index) * 2))
    }

    fn bitmap_pixel(&self, tx: i32, ty: i32) -> Option<u16> {
        let dispcnt = self.reg(0x00);
        let frame = if dispcnt & (1 << 4) != 0 { 0xA000 } else { 0 };
        let (width, height) = if dispcnt & 7 == 5 {
            (160, 128)
        } else {
            (WIDTH as i32, HEIGHT as i32)
        };
        if !(0..width).contains(&tx) || !(0..height).contains(&ty) {
            return None;
        }
        let offset = (ty * width + tx) as usize;
        match dispcnt & 7 {
            3 => Some(self.vram_16(offset * 2) & 0x7FFF),
            4 => {
                let index = self.vram[frame + offset];
                (index != 0).then(|| self.palette_16(usize::from(index) * 2))
            }
            _ => Some(self.vram_16(frame + offset * 2) & 0x7FFF),
        }
    }

    // ----- Objects -----------------------------------------------------

    fn object_layer(&self, line: usize, dispcnt: u16) -> ([ObjPixel; WIDTH], [bool; WIDTH]) {
        let mut pixels = [ObjPixel::default(); WIDTH];
        let mut window = [false; WIDTH];
        let one_dimensional = dispcnt & (1 << 6) != 0;
        let bitmap_mode = (3..=5).contains(&(dispcnt & 7));
        let (.., mosaic_x, mosaic_y) = self.mosaic();

        for index in 0..128 {
            let attr0 = self.oam_16(index * 8);
            let attr1 = self.oam_16(index * 8 + 2);
            let attr2 = self.oam_16(index * 8 + 4);
            let affine = attr0 & (1 << 8) != 0;
            if !affine && attr0 & (1 << 9) != 0 {
                continue; // disabled
            }
            let mode = (attr0 >> 10) & 3;
            if mode == 3 {
                continue;
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

            let mut top = i32::from(attr0 & 0xFF);
            if top >= HEIGHT as i32 {
                top -= 256;
            }
            let mosaic = attr0 & (1 << 12) != 0;
            let sample_line = if mosaic { line - line % mosaic_y } else { line } as i32;
            let row_in_box = sample_line - top;
            if line as i32 - top < 0 || line as i32 - top >= box_height as i32 || row_in_box < 0 {
                continue;
            }
            let mut left = i32::from(attr1 & 0x1FF);
            if left >= 256 {
                left -= 512;
            }

            let tile = u32::from(attr2 & 0x3FF);
            if bitmap_mode && tile < 512 {
                continue; // that VRAM holds the bitmap
            }
            let priority = ((attr2 >> 10) & 3) as u8;
            let eight_bit = attr0 & (1 << 13) != 0;
            let palette = (attr2 >> 12) as u8;

            let (pa, pb, pc, pd) = if affine {
                let group = usize::from((attr1 >> 9) & 0x1F) * 32;
                let p = |n: usize| i32::from(self.oam_16(group + n * 8 + 6) as i16);
                (p(0), p(1), p(2), p(3))
            } else {
                (0x100, 0, 0, 0x100)
            };
            let h_flip = !affine && attr1 & (1 << 12) != 0;
            let v_flip = !affine && attr1 & (1 << 13) != 0;
            let half_box = (box_width as i32 / 2, box_height as i32 / 2);
            let half = (width as i32 / 2, height as i32 / 2);

            for box_x in 0..box_width as i32 {
                let screen_x = left + box_x;
                if !(0..WIDTH as i32).contains(&screen_x) {
                    continue;
                }
                let screen_x = screen_x as usize;
                let sample_x = if mosaic {
                    box_x - (screen_x % mosaic_x) as i32
                } else {
                    box_x
                };
                // Texture coordinate relative to the centre.
                let (dx, dy) = (sample_x - half_box.0, row_in_box - half_box.1);
                let (mut tx, mut ty) = if affine {
                    ((pa * dx + pb * dy) >> 8, (pc * dx + pd * dy) >> 8)
                } else {
                    (dx, dy)
                };
                tx += half.0;
                ty += half.1;
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
                let Some(color) =
                    self.object_texel(tile, eight_bit, palette, one_dimensional, width, tx, ty)
                else {
                    continue;
                };
                if mode == 2 {
                    window[screen_x] = true;
                    continue;
                }
                let current = &mut pixels[screen_x];
                // Lower OAM index wins among objects; a lower priority
                // number wins over that.
                if current.color.is_none() || priority < current.priority {
                    *current = ObjPixel {
                        color: Some(color),
                        priority,
                        semi_transparent: mode == 1,
                    };
                }
            }
        }
        (pixels, window)
    }

    #[allow(clippy::too_many_arguments)] // The texel lookup needs each attribute.
    fn object_texel(
        &self,
        tile: u32,
        eight_bit: bool,
        palette: u8,
        one_dimensional: bool,
        width: u32,
        tx: u32,
        ty: u32,
    ) -> Option<u16> {
        let tile_step = if eight_bit { 2 } else { 1 };
        let row_stride = if one_dimensional {
            width / 8 * tile_step
        } else {
            32
        };
        let tile_number = (tile + (ty / 8) * row_stride + (tx / 8) * tile_step) & 0x3FF;
        let address = 0x1_0000 + tile_number as usize * 32;
        let (row, column) = ((ty % 8) as usize, (tx % 8) as usize);
        let index = if eight_bit {
            self.vram_byte(address + row * 8 + column)
        } else {
            let byte = self.vram_byte(address + row * 4 + column / 2);
            let nibble = if column % 2 == 0 {
                byte & 0xF
            } else {
                byte >> 4
            };
            if nibble == 0 {
                0
            } else {
                palette * 16 + nibble
            }
        };
        (index != 0).then(|| self.palette_16(0x200 + usize::from(index) * 2))
    }

    // ----- Composition -------------------------------------------------

    /// Which layers (bits 0–4) and whether effects (bit 5) are enabled at
    /// each pixel of `line`, by the window covering it.
    fn window_masks(
        &self,
        dispcnt: u16,
        line: usize,
        object_window: &[bool; WIDTH],
    ) -> [u8; WIDTH] {
        if dispcnt & 0xE000 == 0 {
            return [0x3F; WIDTH];
        }
        let span = |value: u16, position: usize, limit: usize| {
            let (start, end) = (
                usize::from(value >> 8),
                usize::from(value & 0xFF).min(limit),
            );
            if start <= end {
                (start..end).contains(&position)
            } else {
                position >= start || position < end
            }
        };
        // Whether window `n` is on and covers this line.
        let on_line =
            |n: usize| dispcnt & (1 << (13 + n)) != 0 && span(self.reg(0x44 + n * 2), line, HEIGHT);
        let (window0, window1) = (on_line(0), on_line(1));
        let (horizontal0, horizontal1) = (self.reg(0x40), self.reg(0x42));
        let winin = self.reg(0x48);
        let winout = self.reg(0x4A);
        std::array::from_fn(|x| {
            if window0 && span(horizontal0, x, WIDTH) {
                (winin & 0x3F) as u8
            } else if window1 && span(horizontal1, x, WIDTH) {
                ((winin >> 8) & 0x3F) as u8
            } else if dispcnt & (1 << 15) != 0 && object_window[x] {
                ((winout >> 8) & 0x3F) as u8
            } else {
                (winout & 0x3F) as u8
            }
        })
    }

    fn compose(
        &self,
        line: usize,
        dispcnt: u16,
        backgrounds: &[Option<Layer>; 4],
        objects: &[ObjPixel; WIDTH],
        object_window: &[bool; WIDTH],
        row: &mut [u32; WIDTH],
    ) {
        let backdrop = self.palette_16(0) & 0x7FFF;
        let bldcnt = self.reg(0x50);
        let bldalpha = self.reg(0x52);
        let eva = u32::from(bldalpha & 0x1F).min(16);
        let evb = u32::from((bldalpha >> 8) & 0x1F).min(16);
        let evy = u32::from(self.reg(0x54) & 0x1F).min(16);
        let priorities: [u16; 4] = std::array::from_fn(|bg| self.reg(0x08 + bg * 2) & 3);
        let masks = self.window_masks(dispcnt, line, object_window);

        for x in 0..WIDTH {
            let mask = masks[x];
            // Keep the two visible layers with the lowest sort key: priority
            // first, then objects before backgrounds, then background number.
            let mut first = (u16::MAX, BACKDROP, backdrop);
            let mut second = first;
            let mut consider = |key: u16, layer: usize, color: u16| {
                if key < first.0 {
                    second = first;
                    first = (key, layer, color);
                } else if key < second.0 {
                    second = (key, layer, color);
                }
            };
            for bg in 0..4 {
                if mask & (1 << bg) != 0
                    && let Some(color) = backgrounds[bg].as_ref().and_then(|layer| layer[x])
                {
                    consider(priorities[bg] * 8 + 1 + bg as u16, bg, color);
                }
            }
            let object = objects[x];
            if mask & (1 << OBJ) != 0
                && let Some(color) = object.color
            {
                consider(u16::from(object.priority) * 8, OBJ, color);
            }

            let effects = mask & (1 << 5) != 0;
            let first_target = bldcnt & (1 << first.1) != 0;
            let second_target = bldcnt & (1 << (8 + second.1)) != 0;
            let semi_transparent = first.1 == OBJ && object.semi_transparent;
            let out = if semi_transparent && second_target {
                alpha(first.2, second.2, eva, evb)
            } else if effects && first_target {
                match (bldcnt >> 6) & 3 {
                    1 if second_target => alpha(first.2, second.2, eva, evb),
                    2 => brightness(first.2, evy, true),
                    3 => brightness(first.2, evy, false),
                    _ => first.2,
                }
            } else {
                first.2
            };
            row[x] = color(out);
        }
    }

    // ----- Memory helpers ----------------------------------------------

    fn vram_byte(&self, offset: usize) -> u8 {
        self.vram.get(offset).copied().unwrap_or(0)
    }

    /// Backgrounds cannot reach object VRAM (0x10000 and up).
    fn bg_vram_byte(&self, offset: usize) -> u8 {
        if offset < 0x1_0000 {
            self.vram[offset]
        } else {
            0
        }
    }

    /// [`Ppu::bg_vram_byte`] for the `N` bytes from `offset`, a row of a
    /// tile: aligned to `N`, so all of them or none are in object VRAM.
    fn bg_tile_bytes<const N: usize>(&self, offset: usize) -> [u8; N] {
        if offset < 0x1_0000 {
            std::array::from_fn(|index| self.vram[offset + index])
        } else {
            [0; N]
        }
    }

    fn vram_16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.vram_byte(offset), self.vram_byte(offset + 1)])
    }

    fn palette_16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.palette[offset], self.palette[offset + 1]])
    }

    fn oam_16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.oam[offset], self.oam[offset + 1]])
    }
}

/// Horizontal mosaic: each block of `size` pixels repeats its leftmost.
fn apply_mosaic(layer: &mut Layer, size: usize) {
    for screen_x in 0..WIDTH {
        if screen_x % size != 0 {
            layer[screen_x] = layer[screen_x - screen_x % size];
        }
    }
}

fn channels(color: u16) -> [u32; 3] {
    [
        u32::from(color & 0x1F),
        u32::from((color >> 5) & 0x1F),
        u32::from((color >> 10) & 0x1F),
    ]
}

fn pack([r, g, b]: [u32; 3]) -> u16 {
    (r.min(31) | g.min(31) << 5 | b.min(31) << 10) as u16
}

fn alpha(first: u16, second: u16, eva: u32, evb: u32) -> u16 {
    let (a, b) = (channels(first), channels(second));
    pack([0, 1, 2].map(|i| (a[i] * eva + b[i] * evb) >> 4))
}

fn brightness(color: u16, evy: u32, brighten: bool) -> u16 {
    pack(channels(color).map(|c| {
        if brighten {
            c + (((31 - c) * evy) >> 4)
        } else {
            c - ((c * evy) >> 4)
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: u16 = 0x001F;
    const GREEN: u16 = 0x03E0;
    const BLUE: u16 = 0x7C00;

    /// A PPU in mode 0 with BG0 showing tile 1 (all colour 1 = red) in the
    /// top-left map cell, on a blue backdrop.
    fn scene() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.write_palette(0, BLUE);
        ppu.write_palette(2, RED);
        for offset in (32..64).step_by(2) {
            ppu.write_vram(offset, 0x1111); // tile 1: every pixel colour 1
        }
        ppu.write_vram(0xF800, 1); // map entry (0, 0) = tile 1
        ppu.write_register(0x08, 31 << 8); // BG0: screen block 31
        ppu.write_register(0x00, 1 << 8); // mode 0, BG0 on
        ppu
    }

    fn draw(ppu: &Ppu, line: usize) -> [u32; WIDTH] {
        let mut row = [0; WIDTH];
        ppu.draw_line(line, &mut row);
        row
    }

    /// A pseudo-random number generator, for filling memory.
    fn next(seed: &mut u32) -> u32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *seed >> 8
    }

    /// Text background `index`'s pixel at `screen_x` of `line`, looked up
    /// on its own: what [`Ppu::text_layer`] does a tile row at a time.
    fn text_pixel(ppu: &Ppu, index: usize, line: usize, screen_x: usize) -> Option<u16> {
        let control = ppu.reg(0x08 + index * 2);
        let scroll_x = usize::from(ppu.reg(0x10 + index * 4) & 0x1FF);
        let scroll_y = usize::from(ppu.reg(0x12 + index * 4) & 0x1FF);
        let char_base = usize::from((control >> 2) & 3) * 0x4000;
        let screen_base = usize::from((control >> 8) & 0x1F) * 0x800;
        let (width, height) =
            [(256, 256), (512, 256), (256, 512), (512, 512)][usize::from(control >> 14)];
        let (mosaic_x, mosaic_y, ..) = ppu.mosaic();
        let mosaic = control & (1 << 6) != 0;
        let line = if mosaic { line - line % mosaic_y } else { line };
        let sample_x = if mosaic {
            screen_x - screen_x % mosaic_x
        } else {
            screen_x
        };
        let (x, y) = ((sample_x + scroll_x) % width, (line + scroll_y) % height);
        let block = x / 256 + (y / 256) * (width / 256);
        let entry =
            ppu.vram_16(screen_base + block * 0x800 + ((y % 256) / 8) * 64 + ((x % 256) / 8) * 2);
        let tile = usize::from(entry & 0x3FF);
        let column = if entry & (1 << 10) != 0 {
            7 - x % 8
        } else {
            x % 8
        };
        let row = if entry & (1 << 11) != 0 {
            7 - y % 8
        } else {
            y % 8
        };
        let color_index = if control & (1 << 7) != 0 {
            ppu.bg_vram_byte(char_base + tile * 64 + row * 8 + column)
        } else {
            let byte = ppu.bg_vram_byte(char_base + tile * 32 + row * 4 + column / 2);
            let nibble = if column % 2 == 0 {
                byte & 0xF
            } else {
                byte >> 4
            };
            if nibble == 0 {
                0
            } else {
                (entry >> 12) as u8 * 16 + nibble
            }
        };
        (color_index != 0).then(|| ppu.palette_16(usize::from(color_index) * 2))
    }

    #[test]
    fn text_layers_by_tile_rows_match_them_pixel_by_pixel() {
        let mut ppu = Ppu::new();
        let mut seed = 1;
        for offset in (0..0x1_8000).step_by(2) {
            ppu.write_vram(offset, next(&mut seed) as u16);
        }
        for offset in (0..0x400).step_by(2) {
            ppu.write_palette(offset, next(&mut seed) as u16);
        }
        for _ in 0..200 {
            // Any size, tile base, map base, colour depth and mosaic.
            ppu.write_register(0x08, next(&mut seed) as u16);
            ppu.write_register(0x10, next(&mut seed) as u16);
            ppu.write_register(0x12, next(&mut seed) as u16);
            ppu.write_register(0x4C, next(&mut seed) as u16);
            for line in 0..HEIGHT {
                let layer = ppu.text_layer(0, line);
                for (x, pixel) in layer.iter().enumerate() {
                    assert_eq!(*pixel, text_pixel(&ppu, 0, line, x), "line {line}, x {x}");
                }
            }
        }
    }

    /// The window mask at pixel `x` of `line`, worked out on its own: what
    /// [`Ppu::window_masks`] does for a whole line.
    fn window_mask(ppu: &Ppu, dispcnt: u16, line: usize, x: usize, object_window: bool) -> u8 {
        if dispcnt & 0xE000 == 0 {
            return 0x3F;
        }
        let span = |value: u16, position: usize, limit: usize| {
            let (start, end) = (
                usize::from(value >> 8),
                usize::from(value & 0xFF).min(limit),
            );
            if start <= end {
                (start..end).contains(&position)
            } else {
                position >= start || position < end
            }
        };
        let inside =
            |h: usize, v: usize| span(ppu.reg(h), x, WIDTH) && span(ppu.reg(v), line, HEIGHT);
        let (winin, winout) = (ppu.reg(0x48), ppu.reg(0x4A));
        if dispcnt & (1 << 13) != 0 && inside(0x40, 0x44) {
            (winin & 0x3F) as u8
        } else if dispcnt & (1 << 14) != 0 && inside(0x42, 0x46) {
            ((winin >> 8) & 0x3F) as u8
        } else if dispcnt & (1 << 15) != 0 && object_window {
            ((winout >> 8) & 0x3F) as u8
        } else {
            (winout & 0x3F) as u8
        }
    }

    #[test]
    fn window_masks_for_a_line_match_them_pixel_by_pixel() {
        let mut ppu = Ppu::new();
        let mut seed = 7;
        for _ in 0..500 {
            for offset in [0x40, 0x42, 0x44, 0x46, 0x48, 0x4A] {
                ppu.write_register(offset, next(&mut seed) as u16);
            }
            let dispcnt = next(&mut seed) as u16 & 0xE000;
            let object_window: [bool; WIDTH] = std::array::from_fn(|_| next(&mut seed) & 1 != 0);
            for line in 0..HEIGHT {
                let masks = ppu.window_masks(dispcnt, line, &object_window);
                for (x, mask) in masks.iter().enumerate() {
                    assert_eq!(*mask, window_mask(&ppu, dispcnt, line, x, object_window[x]));
                }
            }
        }
    }

    #[test]
    fn text_background_draws_its_tile_over_the_backdrop() {
        let row = draw(&scene(), 0);
        assert_eq!(row[0], color(RED));
        assert_eq!(row[7], color(RED));
        assert_eq!(row[8], color(BLUE));
    }

    #[test]
    fn scrolling_moves_the_tile() {
        let mut ppu = scene();
        ppu.write_register(0x10, 4); // BG0HOFS
        let row = draw(&ppu, 0);
        assert_eq!(row[3], color(RED));
        assert_eq!(row[4], color(BLUE));
    }

    /// Adds an 8x8 object at (`x`, 0) using OBJ tile 0 (all colour 1 =
    /// green), with attribute 0 extras `attr0`.
    fn add_object(ppu: &mut Ppu, x: u16, attr0: u16) {
        ppu.write_palette(0x202, GREEN);
        for offset in (0x1_0000..0x1_0020).step_by(2) {
            ppu.write_vram(offset, 0x1111);
        }
        // All-zero OAM entries are visible 8x8 objects at (0, 0); hide the
        // other 127 as games do.
        for entry in 1..128 {
            ppu.write_oam(entry * 8, 1 << 9);
        }
        ppu.write_oam(0, attr0);
        ppu.write_oam(2, x);
        ppu.write_oam(4, 0);
        let dispcnt = ppu.reg(0x00);
        ppu.write_register(0x00, dispcnt | 1 << 12 | 1 << 6);
    }

    #[test]
    fn objects_win_priority_ties_with_backgrounds() {
        let mut ppu = scene();
        add_object(&mut ppu, 4, 0);
        let row = draw(&ppu, 0);
        assert_eq!(row[3], color(RED));
        assert_eq!(row[4], color(GREEN));
        assert_eq!(row[11], color(GREEN));
        assert_eq!(row[12], color(BLUE));
    }

    #[test]
    fn window_hides_a_background_outside_it() {
        let mut ppu = scene();
        ppu.write_register(0x40, 2); // WIN0H: x 0..2
        ppu.write_register(0x44, 160); // WIN0V: all lines
        ppu.write_register(0x48, 0x01); // inside: BG0 only
        ppu.write_register(0x4A, 0x00); // outside: nothing
        let dispcnt = ppu.reg(0x00);
        ppu.write_register(0x00, dispcnt | 1 << 13);
        let row = draw(&ppu, 0);
        assert_eq!(row[1], color(RED));
        assert_eq!(row[2], color(BLUE));
    }

    #[test]
    fn semi_transparent_objects_alpha_blend() {
        let mut ppu = scene();
        add_object(&mut ppu, 0, 1 << 10); // semi-transparent
        ppu.write_register(0x50, 1 << 8); // second target: BG0
        ppu.write_register(0x52, 8 | 8 << 8); // EVA = EVB = 8/16
        let row = draw(&ppu, 0);
        // Half of green and half of red: 15 of 31 in each.
        assert_eq!(row[0], color(15 | 15 << 5));
    }

    #[test]
    fn brightness_moves_towards_white_or_black() {
        assert_eq!(brightness(0, 16, true), 0x7FFF);
        assert_eq!(brightness(0x7FFF, 16, false), 0);
        // Every channel moves halfway (8/16) towards 31.
        assert_eq!(brightness(0x0010, 8, true), 23 | 15 << 5 | 15 << 10);
    }
}
