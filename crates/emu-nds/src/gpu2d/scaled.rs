//! Lines drawn again around a larger 3D layer, for upscaled 3D (NDS-9).
//!
//! The 3D engine can draw its frame `scale` times larger than the screen
//! in each direction. Engine A then draws each line that showed 3D again,
//! `scale` times larger: every other layer keeps its pixels, each spread
//! over a square of `scale` × `scale`, and the pixels of each square are
//! composed again with the larger 3D layer's pixels, through the same
//! windows, priorities, blending and master brightness. Squares where the
//! 3D layer is hidden whatever it holds (behind two layers, or outside its
//! window) take the line's own pixel.
//!
//! Each line is copied as it is drawn ([`Engine2d::copy_line`]), with the
//! registers it was composed with, so the larger lines can be drawn
//! together once the frame is done, on several threads.

use super::compose::{Composer, Layers};
use super::{Engine2d, IS_3D, OPAQUE, WIDTH, layer_3d_pixel, output_pixel};

/// Fills `out`, `scale` rows of `256 * scale` pixels, with `line`'s pixels
/// each spread over a square of `scale` × `scale`.
pub(crate) fn enlarge_line(line: &[u32; WIDTH], scale: usize, out: &mut [u32]) {
    let wide = WIDTH * scale;
    let (first, rest) = out.split_at_mut(wide);
    enlarge_row(line, scale, first);
    for row in rest.chunks_exact_mut(wide) {
        row.copy_from_slice(first);
    }
}

/// Fills one row `256 * scale` pixels wide with `line`'s pixels, each
/// repeated `scale` times.
fn enlarge_row(line: &[u32; WIDTH], scale: usize, out: &mut [u32]) {
    for (block, &pixel) in out.chunks_exact_mut(scale).zip(line) {
        block.fill(pixel);
    }
}

/// One of engine A's lines as drawn, with what drawing it again larger
/// needs.
#[derive(Debug, Clone)]
pub(crate) struct LineCopy {
    /// The line as drawn, `0xFFRRGGBB`.
    native: [u32; WIDTH],
    /// Whether it showed 3D. The fields below matter only if it did.
    showed_3d: bool,
    layers: Layers,
    composer: Composer,
    master_bright: u16,
    /// BG0HOFS.
    scroll: u16,
}

impl Default for LineCopy {
    fn default() -> Self {
        Self {
            native: [0; WIDTH],
            showed_3d: false,
            layers: Layers::new(),
            composer: Composer::default(),
            master_bright: 0,
            scroll: 0,
        }
    }
}

impl Engine2d {
    /// Copies the last line [`Engine2d::render_line`] drew, `native`, into
    /// `copy`, with its layers and registers if it showed 3D.
    pub(crate) fn copy_line(&self, native: &[u32; WIDTH], copy: &mut LineCopy) {
        copy.native = *native;
        copy.showed_3d = false;
        let Some(layers) = self.layers.as_deref().filter(|_| self.showed_3d) else {
            return;
        };
        copy.showed_3d = true;
        copy.layers.clone_from(layers);
        copy.composer = self.composer(layers);
        copy.master_bright = self.registers.master_bright;
        copy.scroll = self.registers.hofs[0];
    }
}

impl LineCopy {
    /// Draws the line `scale` times larger into `out`: `scale` rows of
    /// `256 * scale` pixels. If it showed 3D, `three_d(n)` gives the
    /// larger 3D frame's row for row `n` of `out`, as wide; without one,
    /// the row is the line enlarged.
    pub(crate) fn draw<'a>(
        &self,
        scale: usize,
        three_d: impl Fn(usize) -> Option<&'a [u32]>,
        out: &mut [u32],
    ) {
        if !self.showed_3d {
            return enlarge_line(&self.native, scale, out);
        }
        let (layers, composer) = (&self.layers, &self.composer);
        // Where the 3D layer could show, how each pixel composes with
        // whatever 3D pixel it holds, worked out once: its two front-most
        // layers with an opaque 3D pixel, which of them is the 3D layer's,
        // and its colour when the 3D pixel is transparent.
        let mut around_3d = [None; WIDTH];
        for (x, around) in around_3d.iter_mut().enumerate() {
            let (mask, object) = (layers.window[x], layers.objects[x]);
            let front_with = |three_d: u32| {
                composer.front_layers(mask, object, |bg| {
                    if bg == 0 {
                        three_d
                    } else {
                        layers.backgrounds[bg][x]
                    }
                })
            };
            let front = front_with(OPAQUE | IS_3D);
            if let Some(slot) = front.iter().position(|&(number, _)| number == 0) {
                let transparent = composer.color(mask, front_with(0), object);
                let transparent = output_pixel(self.master_bright, transparent);
                *around = Some((front, slot, transparent));
            }
        }
        let wide = WIDTH * scale;
        // BG0HOFS scrolls the 3D layer within a span of 512 pixels: the
        // frame, then as much again of nothing.
        let span = 2 * wide;
        let scroll = usize::from(self.scroll) * scale;
        for (row, out) in out.chunks_exact_mut(wide).enumerate() {
            let Some(source) = three_d(row).filter(|source| source.len() == wide) else {
                enlarge_row(&self.native, scale, out);
                continue;
            };
            for (x, block) in out.chunks_exact_mut(scale).enumerate() {
                let Some((front, slot, transparent)) = around_3d[x] else {
                    block.fill(self.native[x]);
                    continue;
                };
                // Neighbouring 3D pixels are often alike: compose each
                // run once.
                let mut last: Option<(u32, u32)> = None;
                for (column, pixel) in (x * scale..).zip(block.iter_mut()) {
                    // Below 2 * span, as column < wide and scroll < span.
                    let scrolled = column + scroll;
                    let wrapped = if scrolled >= span {
                        scrolled - span
                    } else {
                        scrolled
                    };
                    let layer = source
                        .get(wrapped)
                        .map_or(0, |&value| layer_3d_pixel(value));
                    *pixel = match last {
                        Some((seen, color)) if seen == layer => color,
                        _ if layer == 0 => transparent,
                        _ => {
                            let mut front = front;
                            front[slot].1 = layer;
                            let composed =
                                composer.color(layers.window[x], front, layers.objects[x]);
                            let color = output_pixel(self.master_bright, composed);
                            last = Some((layer, color));
                            color
                        }
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu2d::compose::tests::random_line;
    use crate::gpu2d::tests::Random;

    /// The pixels of `copy` drawn `scale` times larger over the larger 3D
    /// layer's `rows`, each composed alone with [`Composer::pixel`].
    fn draw_each_pixel(copy: &LineCopy, scale: usize, rows: &[Vec<u32>]) -> Vec<u32> {
        let (layers, composer) = (&copy.layers, &copy.composer);
        let wide = WIDTH * scale;
        let mut out = vec![0; wide * scale];
        for (row, out) in rows.iter().zip(out.chunks_exact_mut(wide)) {
            for (column, pixel) in out.iter_mut().enumerate() {
                let x = column / scale;
                let mut reaches_3d = false;
                composer.pixel(layers.window[x], layers.objects[x], |bg| {
                    reaches_3d |= bg == 0;
                    layers.backgrounds[bg][x]
                });
                if !reaches_3d {
                    *pixel = copy.native[x];
                    continue;
                }
                let scrolled = (column + usize::from(copy.scroll) * scale) % (2 * wide);
                let layer = row.get(scrolled).map_or(0, |&value| layer_3d_pixel(value));
                let composed = composer.pixel(layers.window[x], layers.objects[x], |bg| {
                    if bg == 0 {
                        layer
                    } else {
                        layers.backgrounds[bg][x]
                    }
                });
                *pixel = output_pixel(copy.master_bright, composed);
            }
        }
        out
    }

    #[test]
    fn lines_drawn_larger_match_composing_each_pixel_alone() {
        let mut random = Random(0x5CA1ED);
        for _ in 0..300 {
            let (engine, layers) = random_line(&mut random);
            let scale = 2 + (random.next() % 3) as usize;
            let mut copy = LineCopy {
                native: [0; WIDTH].map(|_| random.next()),
                showed_3d: true,
                composer: engine.composer(&layers),
                layers,
                master_bright: random.next() as u16 & 0xC01F,
                scroll: random.next() as u16 & 0x1FF,
            };
            copy.layers.drawn[0] = true;
            copy.composer = engine.composer(&copy.layers);
            // Runs of alike 3D pixels, as larger frames have, some with
            // alpha 0.
            let rows: Vec<Vec<u32>> = (0..scale)
                .map(|_| {
                    let mut pixel = 0;
                    (0..WIDTH * scale)
                        .map(|_| {
                            if random.next() & 3 == 0 {
                                pixel = random.next() & 0x7F_FFFF;
                            }
                            pixel
                        })
                        .collect()
                })
                .collect();
            let mut out = vec![0; WIDTH * scale * scale];
            copy.draw(scale, |row| rows.get(row).map(Vec::as_slice), &mut out);
            assert_eq!(out, draw_each_pixel(&copy, scale, &rows));
        }
    }
}
