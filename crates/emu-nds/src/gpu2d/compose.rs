//! Windows, layer priority and colour special effects (GBATEK, "LCD I/O
//! Window Feature", "LCD I/O Color Special Effects", "DS Video Stuff" for
//! the DS's window coordinates, "DS 3D Final 2D Output" for 3D blending).
//!
//! Each pixel takes the two front-most layers its window shows (sprites
//! before backgrounds of equal priority, then backgrounds by number, then
//! the backdrop) and applies at most one effect:
//! - a 3D pixel over a 2nd target blends with its own alpha;
//! - a semi-transparent sprite over a 2nd target blends with BLDALPHA, and
//!   a bitmap sprite with its Alpha-OAM value (EVA = alpha + 1, EVB =
//!   15 - alpha; GBATEK does not give the weights);
//! - otherwise BLDCNT's effect applies to a 1st target.
//!
//! A window whose effect bit is clear disables all of them.

use super::color::{self, alpha_blend, blend_3d, brighten, darken};
use super::objects::{ObjBlend, ObjPixel};
use super::{Engine2d, IS_3D, WIDTH};

/// Layer numbers in BLDCNT and window bit order.
const OBJ: usize = 4;
const BACKDROP: usize = 5;
/// No second layer: only the backdrop is behind the first.
const NONE: usize = 6;
/// The window mask bit enabling colour special effects.
const EFFECTS: u8 = 1 << 5;
/// A window mask showing everything.
const ALL: u8 = 0x3F;

/// One line of every layer, ready to compose.
pub(super) struct Layers {
    pub(super) backgrounds: [[u32; WIDTH]; 4],
    /// Which backgrounds were drawn this line.
    pub(super) drawn: [bool; 4],
    pub(super) objects: [ObjPixel; WIDTH],
    /// Per pixel, the layers (bits 0–4) and effects (bit 5) its window
    /// enables.
    pub(super) window: [u8; WIDTH],
}

impl Layers {
    pub(super) fn new() -> Self {
        Self {
            backgrounds: [[0; WIDTH]; 4],
            drawn: [false; 4],
            objects: [ObjPixel::default(); WIDTH],
            window: [ALL; WIDTH],
        }
    }
}

/// Whether `line` is inside a WINxV span: Y1 (high byte) up to but not
/// including Y2 (low byte), wrapping past the bottom when Y1 > Y2. Only the
/// line counter's low 8 bits are compared.
fn in_vertical_span(value: u16, line: u32) -> bool {
    let (y1, y2) = (u32::from(value >> 8), u32::from(value & 0xFF));
    let line = line & 0xFF;
    if y1 <= y2 {
        (y1..y2).contains(&line)
    } else {
        line >= y1 || line < y2
    }
}

/// Sets `bits` over a WINxH span: X1 (high byte) up to but not including
/// X2 (low byte), where X2 = 0 means the right edge. The span wraps when
/// X1 > X2; X1 = X2 is empty.
fn fill_span(window: &mut [u8; WIDTH], value: u16, bits: u8) {
    let (x1, x2) = (usize::from(value >> 8), usize::from(value & 0xFF));
    if x1 == x2 {
        return;
    }
    let end = if x2 == 0 { WIDTH } else { x2 };
    if x1 < end {
        window[x1..end].fill(bits);
    } else {
        window[x1..].fill(bits);
        window[..end].fill(bits);
    }
}

/// A layer pixel's colour as 18-bit colour.
fn rgb666(pixel: u32) -> u32 {
    if pixel & IS_3D != 0 {
        pixel & 0x3_FFFF
    } else {
        color::widen(pixel as u16)
    }
}

/// BLDCNT, BLDALPHA and BLDY, decoded once per line.
struct Effects {
    bldcnt: u32,
    eva: u32,
    evb: u32,
    evy: u32,
}

impl Effects {
    /// The colour of a pixel whose front-most layers are `first` and
    /// `second` (layer number and layer pixel); `object` is the sprite
    /// pixel there. `enabled` is the window's effect bit.
    #[inline]
    fn apply(
        &self,
        enabled: bool,
        (first, first_pixel): (usize, u32),
        (second, second_pixel): (usize, u32),
        object: ObjBlend,
    ) -> u32 {
        let top = rgb666(first_pixel);
        if !enabled {
            return top;
        }
        let second_target = second != NONE && self.bldcnt & (0x100 << second) != 0;
        if second_target {
            if first_pixel & IS_3D != 0 {
                return blend_3d(top, rgb666(second_pixel), (first_pixel >> 18) & 0x1F);
            }
            if first == OBJ {
                match object {
                    ObjBlend::SemiTransparent => {
                        return alpha_blend(top, rgb666(second_pixel), self.eva, self.evb);
                    }
                    ObjBlend::Bitmap(alpha) => {
                        let eva = u32::from(alpha) + 1;
                        return alpha_blend(top, rgb666(second_pixel), eva, 16 - eva);
                    }
                    ObjBlend::Normal => {}
                }
            }
        }
        if self.bldcnt & (1 << first) == 0 {
            return top;
        }
        match (self.bldcnt >> 6) & 3 {
            1 if second_target => alpha_blend(top, rgb666(second_pixel), self.eva, self.evb),
            2 => brighten(top, self.evy),
            3 => darken(top, self.evy),
            _ => top,
        }
    }
}

impl Engine2d {
    /// The window mask for `line`. `object_window` marks the opaque pixels
    /// of OBJ-window sprites.
    pub(super) fn window_line(
        &self,
        line: u32,
        object_window: &[bool; WIDTH],
        window: &mut [u8; WIDTH],
    ) {
        let registers = &self.registers;
        let dispcnt = registers.dispcnt;
        if dispcnt & 0xE000 == 0 {
            window.fill(ALL);
            return;
        }
        window.fill((registers.winout & 0x3F) as u8);
        // The OBJ window needs sprites on too.
        if dispcnt & (1 << 15) != 0 && dispcnt & (1 << 12) != 0 {
            let inside = ((registers.winout >> 8) & 0x3F) as u8;
            for (mask, &marked) in window.iter_mut().zip(object_window) {
                if marked {
                    *mask = inside;
                }
            }
        }
        // Window 1 first, so window 0, which has priority, overwrites it.
        for index in [1, 0] {
            if dispcnt & (1 << (13 + index)) != 0
                && in_vertical_span(registers.window_v[index], line)
            {
                let inside = ((registers.winin >> (8 * index)) & 0x3F) as u8;
                fill_span(window, registers.window_h[index], inside);
            }
        }
    }

    /// Composes `layers` into 18-bit colours.
    pub(super) fn compose(&self, layers: &Layers, out: &mut [u32; WIDTH]) {
        let registers = &self.registers;
        let effects = Effects {
            bldcnt: u32::from(registers.bldcnt),
            eva: u32::from(registers.bldalpha & 0x1F).min(16),
            evb: u32::from((registers.bldalpha >> 8) & 0x1F).min(16),
            evy: u32::from(registers.bldy & 0x1F).min(16),
        };
        let backdrop = self.bg_color(0);
        let priorities = registers.bgcnt.map(|control| (control & 3) as u8);
        // Drawn backgrounds front to back: by priority, then number.
        let mut order = [0; 4];
        let mut count = 0;
        for priority in 0..4 {
            for (bg, (&drawn, &bg_priority)) in layers.drawn.iter().zip(&priorities).enumerate() {
                if drawn && bg_priority == priority {
                    order[count] = bg;
                    count += 1;
                }
            }
        }
        let order = &order[..count];

        for (x, pixel) in out.iter_mut().enumerate() {
            let mask = layers.window[x];
            let object = layers.objects[x];
            let mut top = [(BACKDROP, backdrop), (NONE, 0)];
            let mut found = 0;
            let mut object_pending = object.color != 0 && mask & (1 << OBJ) != 0;
            for &bg in order {
                if object_pending && object.priority <= priorities[bg] {
                    object_pending = false;
                    top[found] = (OBJ, object.color);
                    found += 1;
                    if found == 2 {
                        break;
                    }
                }
                let color = layers.backgrounds[bg][x];
                if color != 0 && mask & (1 << bg) != 0 {
                    top[found] = (bg, color);
                    found += 1;
                    if found == 2 {
                        break;
                    }
                }
            }
            if object_pending && found < 2 {
                top[found] = (OBJ, object.color);
                found += 1;
            }
            if found < 2 {
                top[found] = (BACKDROP, backdrop);
            }
            *pixel = effects.apply(mask & EFFECTS != 0, top[0], top[1], object.blend);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(value: u16) -> Vec<usize> {
        let mut window = [0; WIDTH];
        fill_span(&mut window, value, 1);
        (0..WIDTH).filter(|&x| window[x] == 1).collect()
    }

    #[test]
    fn horizontal_spans_end_before_x2() {
        assert_eq!(span(0x0204), vec![2, 3]);
    }

    #[test]
    fn x2_of_zero_means_the_right_edge() {
        assert_eq!(span(0xFE00), vec![254, 255]);
        assert!(span(0x0000).is_empty(), "X1 = X2 = 0 shows no window");
    }

    #[test]
    fn horizontal_spans_wrap_when_x1_exceeds_x2() {
        assert_eq!(span(0xFE02), vec![0, 1, 254, 255]);
    }

    #[test]
    fn vertical_spans_wrap_when_y1_exceeds_y2() {
        assert!(in_vertical_span(0x10C0, 16));
        assert!(!in_vertical_span(0x10C0, 15));
        assert!(!in_vertical_span(0x10C0, 192));
        assert!(in_vertical_span(0xB010, 5));
        assert!(in_vertical_span(0xB010, 180));
        assert!(!in_vertical_span(0xB010, 100));
    }
}
