//! Draws one line (GBATEK, "LCD VRAM Overview", "LCD VRAM BG Screen Data
//! Format", "LCD OBJ - OAM Attributes", "LCD I/O Window Feature", "LCD I/O
//! Color Special Effects", "LCD I/O Mosaic Function").
//!
//! Each layer (four backgrounds and the objects) is drawn into its own line
//! buffer of BGR555 colours with transparency; then every pixel picks the
//! top two visible layers by priority, inside whatever window covers it,
//! and blends them.

use super::{HEIGHT, Ppu, WIDTH, color};

/// Sentinel for a transparent pixel in a layer buffer (BGR555 uses bits 0–14).
const TRANSPARENT: u16 = 0x8000;
/// Mask for a 15-bit BGR555 colour.
const COLOR_MASK: u16 = 0x7FFF;

/// Background or object pixel: BGR555 colour (`0..=0x7FFF`), or [`TRANSPARENT`].
type Layer = [u16; WIDTH];

/// An object pixel with what composition needs to know.
#[derive(Debug, Clone, Copy)]
struct ObjPixel {
    color: u16,
    priority: u8,
    semi_transparent: bool,
}

impl Default for ObjPixel {
    fn default() -> Self {
        Self {
            color: TRANSPARENT,
            priority: 0,
            semi_transparent: false,
        }
    }
}

/// Where a text background's map and tiles are, and their format.
struct TextMap {
    screen_base: usize,
    char_base: usize,
    eight_bit: bool,
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
    #[inline]
    fn reg(&self, offset: usize) -> u16 {
        self.register16(offset)
    }

    /// Draws line `line` into `row` as `0xFFRRGGBB`.
    pub(super) fn draw_line(&self, line: usize, row: &mut [u32; WIDTH]) {
        let dispcnt = self.reg(0x00);
        let mode = dispcnt & 7;
        let mut backgrounds = [[TRANSPARENT; WIDTH]; 4];
        let mut bg_mask = 0u8;
        for (index, layer) in backgrounds.iter_mut().enumerate() {
            if dispcnt & (1 << (8 + index)) == 0 {
                continue;
            }
            let drawn = match (mode, index) {
                (0, _) | (1, 0 | 1) => {
                    self.text_layer_into(index, line, layer);
                    true
                }
                (1 | 2, 2) | (2, 3) => {
                    self.affine_layer_into(index, false, layer);
                    true
                }
                (3..=5, 2) => {
                    self.affine_layer_into(index, true, layer);
                    true
                }
                _ => false,
            };
            if drawn {
                bg_mask |= 1 << index;
            }
        }
        if dispcnt & (1 << 12) != 0 {
            let (objects, object_window, any_semi) = self.object_layer(line, dispcnt);
            self.compose(
                line,
                dispcnt,
                &backgrounds,
                bg_mask,
                Some((&objects, &object_window)),
                any_semi,
                row,
            );
        } else {
            self.compose(line, dispcnt, &backgrounds, bg_mask, None, false, row);
        }
    }

    // ----- Backgrounds -------------------------------------------------

    fn mosaic(&self) -> (usize, usize, usize, usize) {
        let mosaic = self.reg(0x4C);
        let size = |shift: u16| usize::from((mosaic >> shift) & 0xF) + 1;
        (size(0), size(4), size(8), size(12))
    }

    #[cfg(test)]
    fn text_layer(&self, index: usize, line: usize) -> Layer {
        let mut layer = [TRANSPARENT; WIDTH];
        self.text_layer_into(index, line, &mut layer);
        layer
    }

    fn text_layer_into(&self, index: usize, line: usize, layer: &mut Layer) {
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
        let y = (line + scroll_y) & (height - 1);
        let map = TextMap {
            screen_base,
            char_base,
            eight_bit,
        };

        // Whole tile rows, from the one under the left edge: a tile more
        // than the screen, starting `skip` pixels left of it.
        let skip = scroll_x & 7;
        let base_x = scroll_x - skip;
        let width_mask = width - 1;
        let block_y_offset = (y >> 8) * (width >> 8) * 0x800 + ((y & 0xFF) >> 3) * 64;
        let y_in_tile = y & 7;
        let mut buf = [TRANSPARENT; WIDTH + 8];
        for tile in 0..(WIDTH / 8 + 1) {
            let x = (base_x + tile * 8) & width_mask;
            let entry_offset =
                map.screen_base + block_y_offset + (x >> 8) * 0x800 + ((x & 0xFF) >> 3) * 2;
            let dst = &mut buf[tile * 8..tile * 8 + 8];
            self.write_text_tile_row(&map, entry_offset, y_in_tile, dst);
        }
        layer.copy_from_slice(&buf[skip..skip + WIDTH]);
        if mosaic {
            apply_mosaic(layer, mosaic_x);
        }
    }

    /// Writes the eight pixels of tile row `y_in_tile` into `dst`, left to
    /// right after flipping, leaving transparent pixels as [`TRANSPARENT`].
    #[inline]
    fn write_text_tile_row(
        &self,
        map: &TextMap,
        entry_offset: usize,
        y_in_tile: usize,
        dst: &mut [u16],
    ) {
        let entry = self.vram_16(entry_offset);
        let tile = usize::from(entry & 0x3FF);
        let row = if entry & (1 << 11) != 0 {
            7 - y_in_tile
        } else {
            y_in_tile
        };
        let hflip = entry & (1 << 10) != 0;
        if map.eight_bit {
            let offset = map.char_base + tile * 64 + row * 8;
            if offset >= 0x1_0000 {
                return;
            }
            let bytes = &self.vram[offset..offset + 8];
            for (i, &index) in bytes.iter().enumerate() {
                if index != 0 {
                    let slot = if hflip { 7 - i } else { i };
                    dst[slot] = self.palette_16(usize::from(index) * 2) & COLOR_MASK;
                }
            }
        } else {
            let offset = map.char_base + tile * 32 + row * 4;
            if offset >= 0x1_0000 {
                return;
            }
            let bytes = &self.vram[offset..offset + 4];
            if bytes == [0, 0, 0, 0] {
                return;
            }
            let palette_base = usize::from(entry >> 12) * 32;
            for (pair, &byte) in bytes.iter().enumerate() {
                if byte == 0 {
                    continue;
                }
                let lo = usize::from(byte & 0xF);
                if lo != 0 {
                    let slot = if hflip { 7 - pair * 2 } else { pair * 2 };
                    dst[slot] = self.palette_16(palette_base + lo * 2) & COLOR_MASK;
                }
                let hi = usize::from(byte >> 4);
                if hi != 0 {
                    let slot = if hflip { 6 - pair * 2 } else { pair * 2 + 1 };
                    dst[slot] = self.palette_16(palette_base + hi * 2) & COLOR_MASK;
                }
            }
        }
    }

    /// Affine background (modes 1–2) or bitmap (modes 3–5) for BG2/BG3,
    /// sampled through the rotation/scaling matrix.
    fn affine_layer_into(&self, index: usize, bitmap: bool, layer: &mut Layer) {
        let control = self.reg(0x08 + index * 2);
        let base = 0x20 + (index - 2) * 0x10;
        let pa = i32::from(self.reg(base) as i16);
        let pc = i32::from(self.reg(base + 4) as i16);
        let (mut x, mut y) = self.affine_reference[index - 2];
        let (mosaic_x, ..) = self.mosaic();
        let mosaic = control & (1 << 6) != 0;
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
            apply_mosaic(layer, mosaic_x);
        }
    }

    fn affine_tile_pixel(&self, control: u16, tx: i32, ty: i32) -> u16 {
        let size = 128 << (control >> 14);
        let wrap = control & (1 << 13) != 0;
        let (tx, ty) = if wrap {
            (tx & (size - 1), ty & (size - 1))
        } else if (0..size).contains(&tx) && (0..size).contains(&ty) {
            (tx, ty)
        } else {
            return TRANSPARENT;
        };
        let char_base = usize::from((control >> 2) & 3) * 0x4000;
        let screen_base = usize::from((control >> 8) & 0x1F) * 0x800;
        let (tx, ty) = (tx as usize, ty as usize);
        let tiles_per_row = size as usize / 8;
        let tile = usize::from(self.bg_vram_byte(screen_base + (ty / 8) * tiles_per_row + tx / 8));
        let index = self.bg_vram_byte(char_base + tile * 64 + (ty % 8) * 8 + tx % 8);
        if index != 0 {
            self.palette_16(usize::from(index) * 2) & COLOR_MASK
        } else {
            TRANSPARENT
        }
    }

    fn bitmap_pixel(&self, tx: i32, ty: i32) -> u16 {
        let dispcnt = self.reg(0x00);
        let frame = if dispcnt & (1 << 4) != 0 { 0xA000 } else { 0 };
        let (width, height) = if dispcnt & 7 == 5 {
            (160, 128)
        } else {
            (WIDTH as i32, HEIGHT as i32)
        };
        if !(0..width).contains(&tx) || !(0..height).contains(&ty) {
            return TRANSPARENT;
        }
        let offset = (ty * width + tx) as usize;
        match dispcnt & 7 {
            3 => self.vram_16(offset * 2) & COLOR_MASK,
            4 => {
                let index = self.vram[frame + offset];
                if index != 0 {
                    self.palette_16(usize::from(index) * 2) & COLOR_MASK
                } else {
                    TRANSPARENT
                }
            }
            _ => self.vram_16(frame + offset * 2) & COLOR_MASK,
        }
    }

    // ----- Objects -----------------------------------------------------

    fn object_layer(&self, line: usize, dispcnt: u16) -> ([ObjPixel; WIDTH], [bool; WIDTH], bool) {
        let mut pixels = [ObjPixel::default(); WIDTH];
        let mut window = [false; WIDTH];
        let mut any_semi = false;
        let one_dimensional = dispcnt & (1 << 6) != 0;
        let bitmap_mode = (3..=5).contains(&(dispcnt & 7));
        let (.., mosaic_x, mosaic_y) = self.mosaic();

        for index in 0..128 {
            let attr0 = self.oam_16(index * 8);
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
            let attr1 = self.oam_16(index * 8 + 2);
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
            let start_x = (-left).max(0);
            let end_x = (box_width as i32).min(WIDTH as i32 - left);
            if start_x >= end_x {
                continue;
            }

            let attr2 = self.oam_16(index * 8 + 4);
            let tile = u32::from(attr2 & 0x3FF);
            if bitmap_mode && tile < 512 {
                continue; // that VRAM holds the bitmap
            }
            let priority = ((attr2 >> 10) & 3) as u8;
            let eight_bit = attr0 & (1 << 13) != 0;
            let palette = (attr2 >> 12) as u8;

            if !affine && !mosaic {
                let h_flip = attr1 & (1 << 12) != 0;
                let v_flip = attr1 & (1 << 13) != 0;
                let ty = if v_flip {
                    height - 1 - row_in_box as u32
                } else {
                    row_in_box as u32
                };
                for box_x in start_x..end_x {
                    let screen_x = (left + box_x) as usize;
                    let tx = if h_flip {
                        width - 1 - box_x as u32
                    } else {
                        box_x as u32
                    };
                    let color =
                        self.object_texel(tile, eight_bit, palette, one_dimensional, width, tx, ty);
                    if color == TRANSPARENT {
                        continue;
                    }
                    if mode == 2 {
                        window[screen_x] = true;
                        continue;
                    }
                    let current = &mut pixels[screen_x];
                    if current.color == TRANSPARENT || priority < current.priority {
                        let semi = mode == 1;
                        any_semi |= semi;
                        *current = ObjPixel {
                            color,
                            priority,
                            semi_transparent: semi,
                        };
                    }
                }
                continue;
            }

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

            for box_x in start_x..end_x {
                let screen_x = (left + box_x) as usize;
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
                let color =
                    self.object_texel(tile, eight_bit, palette, one_dimensional, width, tx, ty);
                if color == TRANSPARENT {
                    continue;
                }
                if mode == 2 {
                    window[screen_x] = true;
                    continue;
                }
                let current = &mut pixels[screen_x];
                // Lower OAM index wins among objects; a lower priority
                // number wins over that.
                if current.color == TRANSPARENT || priority < current.priority {
                    let semi = mode == 1;
                    any_semi |= semi;
                    *current = ObjPixel {
                        color,
                        priority,
                        semi_transparent: semi,
                    };
                }
            }
        }
        (pixels, window, any_semi)
    }

    #[allow(clippy::too_many_arguments)] // The texel lookup needs each attribute.
    #[inline]
    fn object_texel(
        &self,
        tile: u32,
        eight_bit: bool,
        palette: u8,
        one_dimensional: bool,
        width: u32,
        tx: u32,
        ty: u32,
    ) -> u16 {
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
            let nibble = if column & 1 == 0 {
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
        if index != 0 {
            self.palette_16(0x200 + usize::from(index) * 2) & COLOR_MASK
        } else {
            TRANSPARENT
        }
    }

    // ----- Composition -------------------------------------------------

    /// Which layers (bits 0–4) and whether effects (bit 5) are enabled at
    /// each pixel of `line`, by the window covering it.
    fn window_masks(
        &self,
        dispcnt: u16,
        line: usize,
        object_window: Option<&[bool; WIDTH]>,
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
        let obj_win_on = dispcnt & (1 << 15) != 0;
        std::array::from_fn(|x| {
            if window0 && span(horizontal0, x, WIDTH) {
                (winin & 0x3F) as u8
            } else if window1 && span(horizontal1, x, WIDTH) {
                ((winin >> 8) & 0x3F) as u8
            } else if obj_win_on && object_window.is_some_and(|w| w[x]) {
                ((winout >> 8) & 0x3F) as u8
            } else {
                (winout & 0x3F) as u8
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn compose(
        &self,
        line: usize,
        dispcnt: u16,
        backgrounds: &[Layer; 4],
        bg_mask: u8,
        objects: Option<(&[ObjPixel; WIDTH], &[bool; WIDTH])>,
        any_semi_transparent: bool,
        row: &mut [u32; WIDTH],
    ) {
        let backdrop = self.palette_16(0) & COLOR_MASK;
        let bldcnt = self.reg(0x50);
        let blend_mode = (bldcnt >> 6) & 3;
        let priorities: [u16; 4] = std::array::from_fn(|bg| self.reg(0x08 + bg * 2) & 3);

        // Active backgrounds sorted in front-to-back order (lowest priority,
        // then lowest BG number).
        let mut active = [0usize; 4];
        let mut active_len = 0usize;
        for priority in 0..4u16 {
            for (bg, &p) in priorities.iter().enumerate() {
                if bg_mask & (1 << bg) != 0 && p == priority {
                    active[active_len] = bg;
                    active_len += 1;
                }
            }
        }
        let active = &active[..active_len];

        // Fast path: no windows, no color effects, and no semi-transparent
        // objects, so only the single front-most opaque pixel matters.
        if dispcnt & 0xE000 == 0 && blend_mode == 0 && !any_semi_transparent {
            let backdrop_argb = color(backdrop);
            match (objects, active) {
                (None, []) => row.fill(backdrop_argb),
                (None, &[bg]) => {
                    let layer = &backgrounds[bg];
                    for (out, &px) in row.iter_mut().zip(layer) {
                        *out = if px != TRANSPARENT {
                            color(px)
                        } else {
                            backdrop_argb
                        };
                    }
                }
                (None, bgs) => {
                    for (x, out) in row.iter_mut().enumerate() {
                        let mut chosen = backdrop;
                        for &bg in bgs {
                            let px = backgrounds[bg][x];
                            if px != TRANSPARENT {
                                chosen = px;
                                break;
                            }
                        }
                        *out = color(chosen);
                    }
                }
                (Some((objs, _)), bgs) => {
                    for (x, out) in row.iter_mut().enumerate() {
                        let obj = objs[x];
                        let mut chosen = if obj.color != TRANSPARENT {
                            obj.color
                        } else {
                            backdrop
                        };
                        let obj_prio = if obj.color != TRANSPARENT {
                            u16::from(obj.priority)
                        } else {
                            4
                        };
                        for &bg in bgs {
                            // Objects beat backgrounds of equal priority.
                            if priorities[bg] >= obj_prio {
                                break;
                            }
                            let px = backgrounds[bg][x];
                            if px != TRANSPARENT {
                                chosen = px;
                                break;
                            }
                        }
                        *out = color(chosen);
                    }
                }
            }
            return;
        }

        let bldalpha = self.reg(0x52);
        let eva = u32::from(bldalpha & 0x1F).min(16);
        let evb = u32::from((bldalpha >> 8) & 0x1F).min(16);
        let evy = u32::from(self.reg(0x54) & 0x1F).min(16);
        let masks = self.window_masks(dispcnt, line, objects.map(|(_, w)| w));

        for (x, out_pixel) in row.iter_mut().enumerate() {
            let mask = masks[x];
            // Keep the two visible layers with the lowest sort key: priority
            // first, then objects before backgrounds, then background number.
            let mut first = (u16::MAX, BACKDROP, backdrop);
            let mut second = first;
            let mut consider = |key: u16, layer: usize, c: u16| {
                if key < first.0 {
                    second = first;
                    first = (key, layer, c);
                } else if key < second.0 {
                    second = (key, layer, c);
                }
            };
            for &bg in active {
                if mask & (1 << bg) != 0 {
                    let px = backgrounds[bg][x];
                    if px != TRANSPARENT {
                        consider(priorities[bg] * 8 + 1 + bg as u16, bg, px);
                    }
                }
            }
            let object = objects.map_or(ObjPixel::default(), |(objs, _)| objs[x]);
            if mask & (1 << OBJ) != 0 && object.color != TRANSPARENT {
                consider(u16::from(object.priority) * 8, OBJ, object.color);
            }

            let effects = mask & (1 << 5) != 0;
            let first_target = bldcnt & (1 << first.1) != 0;
            let second_target = bldcnt & (1 << (8 + second.1)) != 0;
            let semi_transparent = first.1 == OBJ && object.semi_transparent;
            let out = if semi_transparent && second_target {
                alpha(first.2, second.2, eva, evb)
            } else if effects && first_target {
                match blend_mode {
                    1 if second_target => alpha(first.2, second.2, eva, evb),
                    2 => brightness(first.2, evy, true),
                    3 => brightness(first.2, evy, false),
                    _ => first.2,
                }
            } else {
                first.2
            };
            *out_pixel = color(out);
        }
    }

    // ----- Memory helpers ----------------------------------------------

    #[inline]
    fn vram_byte(&self, offset: usize) -> u8 {
        self.vram.get(offset).copied().unwrap_or(0)
    }

    /// Backgrounds cannot reach object VRAM (0x10000 and up).
    #[inline]
    fn bg_vram_byte(&self, offset: usize) -> u8 {
        if offset < 0x1_0000 {
            self.vram[offset]
        } else {
            0
        }
    }

    #[inline]
    fn vram_16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.vram_byte(offset), self.vram_byte(offset + 1)])
    }

    #[inline]
    fn palette_16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.palette[offset], self.palette[offset + 1]])
    }

    #[inline]
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
                    let got = (*pixel != TRANSPARENT).then_some(*pixel);
                    let expected = text_pixel(&ppu, 0, line, x).map(|c| c & COLOR_MASK);
                    assert_eq!(got, expected, "line {line}, x {x}");
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
                let masks = ppu.window_masks(dispcnt, line, Some(&object_window));
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
