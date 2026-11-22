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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Layers {
    /// Each drawn background's line. The others keep an older line, which
    /// is never read.
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

    /// Readies the layers for a new line. Only the sprites need emptying:
    /// a drawn background fills its whole line and an undrawn one is never
    /// read, and the window mask is drawn whole every line.
    pub(super) fn clear(&mut self) {
        self.drawn = [false; 4];
        self.objects.fill(ObjPixel::default());
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Effects {
    bldcnt: u32,
    eva: u32,
    evb: u32,
    evy: u32,
}

impl Effects {
    /// The layers an effect could change the colour of when in front,
    /// bit n for layer n: the 1st targets of a brightness change, or of
    /// alpha blending when it has a 2nd target, and with any 2nd target,
    /// BG0 (whose 3D pixels blend) and the sprites (whose semi-transparent
    /// and bitmap pixels blend).
    fn changeable_layers(&self) -> u32 {
        let first_targets = self.bldcnt & 0x3F;
        let second_targets = self.bldcnt & 0x3F00 != 0;
        let mut layers = match (self.bldcnt >> 6) & 3 {
            1 if second_targets => first_targets,
            2 | 3 => first_targets,
            _ => 0,
        };
        if second_targets {
            layers |= 1 | 1 << OBJ;
        }
        layers
    }

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

/// What composing a line's pixels needs from the registers: the effects,
/// the backdrop, and the drawn backgrounds in front-to-back order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Composer {
    effects: Effects,
    backdrop: u32,
    priorities: [u8; 4],
    order: [usize; 4],
    count: usize,
}

impl Composer {
    /// The colour of a pixel, as 18-bit colour, whose window enables the
    /// layers and effects in `mask`, where the sprite pixel is `object`
    /// and `background(n)` gives BG `n`'s layer pixel: one pixel composed
    /// alone, which the tests hold the faster ways of composing to.
    #[cfg(test)]
    pub(super) fn pixel(
        &self,
        mask: u8,
        object: ObjPixel,
        background: impl FnMut(usize) -> u32,
    ) -> u32 {
        self.color(mask, self.front_layers(mask, object, background), object)
    }

    /// The two front-most layers of a pixel whose window enables the
    /// layers in `mask`, where the sprite pixel is `object` and
    /// `background(n)` gives BG `n`'s layer pixel: layer number and layer
    /// pixel, with the backdrop behind the layers found and [`NONE`]
    /// behind the backdrop. `background` is called only for backgrounds
    /// that could show: drawn, enabled by the window and not already behind
    /// two layers. Whether BG `n` is among them depends on its pixel only
    /// through whether that is 0.
    #[inline]
    pub(super) fn front_layers(
        &self,
        mask: u8,
        object: ObjPixel,
        mut background: impl FnMut(usize) -> u32,
    ) -> [(usize, u32); 2] {
        let mut top = [(BACKDROP, self.backdrop), (NONE, 0)];
        let mut found = 0;
        let mut object_pending = object.color != 0 && mask & (1 << OBJ) != 0;
        for &bg in &self.order[..self.count] {
            if object_pending && object.priority <= self.priorities[bg] {
                object_pending = false;
                top[found] = (OBJ, object.color);
                found += 1;
                if found == 2 {
                    return top;
                }
            }
            if mask & (1 << bg) == 0 {
                continue;
            }
            let color = background(bg);
            if color != 0 {
                top[found] = (bg, color);
                found += 1;
                if found == 2 {
                    return top;
                }
            }
        }
        if object_pending {
            top[found] = (OBJ, object.color);
            found += 1;
        }
        if found < 2 {
            top[found] = (BACKDROP, self.backdrop);
        }
        top
    }

    /// The colour, as 18-bit colour, of a pixel whose window mask is
    /// `mask`, whose two front-most layers are `top` and whose sprite pixel
    /// is `object`.
    pub(super) fn color(
        &self,
        mask: u8,
        [first, second]: [(usize, u32); 2],
        object: ObjPixel,
    ) -> u32 {
        self.effects
            .apply(mask & EFFECTS != 0, first, second, object.blend)
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

    /// The composer for a line whose drawn backgrounds `layers` lists,
    /// with the registers as they are now.
    pub(super) fn composer(&self, layers: &Layers) -> Composer {
        let registers = &self.registers;
        let effects = Effects {
            bldcnt: u32::from(registers.bldcnt),
            eva: u32::from(registers.bldalpha & 0x1F).min(16),
            evb: u32::from((registers.bldalpha >> 8) & 0x1F).min(16),
            evy: u32::from(registers.bldy & 0x1F).min(16),
        };
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
        Composer {
            effects,
            backdrop: self.bg_color(0),
            priorities,
            order,
            count,
        }
    }

    /// Composes `layers` into 18-bit colours a layer at a time: each layer
    /// is stacked over the line from back to front, keeping the two
    /// front-most layers of every pixel (those [`Composer::front_layers`]
    /// finds), then the effects apply.
    pub(super) fn compose(&self, layers: &Layers, out: &mut [u32; WIDTH]) {
        let composer = self.composer(layers);
        let mut stack = Stack::new(composer.backdrop);
        let window = &layers.window;
        let objects = &layers.objects;
        let object_priorities = objects
            .iter()
            .filter(|object| object.color != 0)
            .fold(0u8, |priorities, object| priorities | 1 << object.priority);
        // Sprites go in front of backgrounds of their priority or behind,
        // and among backgrounds of one priority, lower numbers in front.
        let drawn = &composer.order[..composer.count];
        let mut sprites = [0; WIDTH];
        for priority in (0..4).rev() {
            for &bg in drawn.iter().rev() {
                if composer.priorities[bg] == priority {
                    stack.push(bg, &layers.backgrounds[bg], window);
                }
            }
            if object_priorities & (1 << priority) != 0 {
                for (color, object) in sprites.iter_mut().zip(objects) {
                    *color = if object.priority == priority {
                        object.color
                    } else {
                        0
                    };
                }
                stack.push(OBJ, &sprites, window);
            }
        }
        let effects = &composer.effects;
        let changeable = effects.changeable_layers();
        if changeable == 0 {
            for (pixel, &first) in out.iter_mut().zip(&stack.first) {
                *pixel = rgb666(Stack::unpack(first).1);
            }
            return;
        }
        for ((((pixel, &first), &second), &mask), object) in out
            .iter_mut()
            .zip(&stack.first)
            .zip(&stack.second)
            .zip(window)
            .zip(objects)
        {
            let (layer, front) = Stack::unpack(first);
            *pixel = if mask & EFFECTS != 0 && changeable & (1 << layer) != 0 {
                effects.apply(true, (layer, front), Stack::unpack(second), object.blend)
            } else {
                rgb666(front)
            };
        }
    }
}

/// The two front-most layers of every pixel of a line, as layers are
/// stacked from back to front. Each is a layer pixel with its layer number
/// in bits 24–26, which layer pixels leave clear.
struct Stack {
    first: [u32; WIDTH],
    second: [u32; WIDTH],
}

impl Stack {
    /// Where a stacked pixel keeps its layer number.
    const LAYER_SHIFT: u32 = 24;
    const LAYER_MASK: u32 = 7 << Self::LAYER_SHIFT;

    /// Only the backdrop, with nothing behind it.
    fn new(backdrop: u32) -> Self {
        Self {
            first: [Self::pack(BACKDROP, backdrop); WIDTH],
            second: [Self::pack(NONE, 0); WIDTH],
        }
    }

    fn pack(layer: usize, pixel: u32) -> u32 {
        (layer as u32) << Self::LAYER_SHIFT | pixel
    }

    /// A stacked pixel's layer number and layer pixel.
    fn unpack(stacked: u32) -> (usize, u32) {
        (
            (stacked >> Self::LAYER_SHIFT & 7) as usize,
            stacked & !Self::LAYER_MASK,
        )
    }

    /// Stacks layer `layer`'s line, `pixels`, in front where a pixel is
    /// not 0 and its `window` mask shows the layer.
    fn push(&mut self, layer: usize, pixels: &[u32; WIDTH], window: &[u8; WIDTH]) {
        let bit = 1 << layer;
        for (((first, second), &pixel), &mask) in self
            .first
            .iter_mut()
            .zip(&mut self.second)
            .zip(pixels)
            .zip(window)
        {
            let shown = (pixel != 0) & (mask & bit != 0);
            // All ones where the stack stays as it was. Choosing with a
            // mask stores both values every time, so the loop vectorizes;
            // written as `if`, the compiler stores only where the layer
            // shows, a branch per pixel.
            let keep = u32::from(!shown).wrapping_neg();
            let (front, behind) = (*first, *second);
            *second = (front & !keep) | (behind & keep);
            *first = (Self::pack(layer, pixel) & !keep) | (front & keep);
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::gpu2d::tests::{Random, layer_pixel};

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

    /// A line of random layers to compose, and an engine with random
    /// layer priorities, effects and backdrop to compose it.
    pub(in crate::gpu2d) fn random_line(random: &mut Random) -> (Engine2d, Layers) {
        let mut engine = Engine2d::new(crate::vram::Engine::A);
        for offset in [0x08, 0x0A, 0x0C, 0x0E, 0x50, 0x52, 0x54] {
            engine.write_register(offset, random.next() as u16);
        }
        engine.write_palette(0, random.next() as u16);
        let mut layers = Layers::new();
        layers.drawn = [0, 1, 2, 3].map(|_| random.next() & 1 != 0);
        let few_sprites = random.next() & 1 != 0;
        let windows = random.next() & 1 != 0;
        for x in 0..WIDTH {
            for (bg, background) in layers.backgrounds.iter_mut().enumerate() {
                background[x] = layer_pixel(random, bg == 0);
            }
            let object = random.next();
            if !few_sprites || object & 7 == 0 {
                layers.objects[x] = ObjPixel {
                    color: layer_pixel(random, false),
                    priority: (object >> 4) as u8 & 3,
                    blend: match (object >> 8) & 3 {
                        0 => ObjBlend::Normal,
                        1 => ObjBlend::SemiTransparent,
                        _ => ObjBlend::Bitmap((object >> 12) as u8 & 15),
                    },
                };
            }
            if windows {
                layers.window[x] = random.next() as u8 & ALL;
            }
        }
        (engine, layers)
    }

    #[test]
    fn composing_a_layer_at_a_time_matches_composing_each_pixel() {
        let mut random = Random(0x2D_C0DE);
        for _ in 0..2000 {
            let (engine, layers) = random_line(&mut random);
            let mut composed = [0; WIDTH];
            engine.compose(&layers, &mut composed);
            let composer = engine.composer(&layers);
            for (x, &color) in composed.iter().enumerate() {
                let expected = composer.pixel(layers.window[x], layers.objects[x], |bg| {
                    layers.backgrounds[bg][x]
                });
                assert_eq!(color, expected, "pixel {x}");
            }
        }
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
