//! Whole-frame passes after the polygons are drawn: edge marking, fog and
//! anti-aliasing (GBATEK, "DS 3D Toon, Edge, Fog, Alpha-Blending,
//! Anti-Aliasing").

use super::pixel::{Buffers, PIXELS};
use super::raster::{HEIGHT, WIDTH};
use crate::gpu3d::color;
use crate::gpu3d::registers::RenderConfig;

/// An anti-aliased edge that edge marking paints is blended about half
/// with the pixel beneath, whatever it covered (GBATEK guesses "alpha=16
/// or so"; melonDS uses this).
const MARKED_COVERAGE: u8 = 16;

/// Paints the edges of opaque polygons in the edge colour for their
/// polygon ID: where a neighbour (up, down, left, right) has another ID
/// and is further away. Beyond the screen, neighbours are the rear plane's
/// ID and depth.
pub(crate) fn mark_edges(buffers: &mut Buffers, config: &RenderConfig) {
    let clear_id = ((config.clear_color >> 24) & 0x3F) as u8;
    let edge_colors = config.edge_colors.map(|c| color::rgb6(u32::from(c)));
    let neighbour = |buffers: &Buffers, x: Option<usize>, y: Option<usize>| match (x, y) {
        (Some(x), Some(y)) if x < WIDTH && y < HEIGHT => {
            let index = y * WIDTH + x;
            (buffers.attrs[index].opaque_id, buffers.depth[index])
        }
        _ => (clear_id, config.clear_depth),
    };
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = y * WIDTH + x;
            let attr = buffers.attrs[index];
            if !attr.on_edge() {
                continue;
            }
            let here = buffers.depth[index];
            let marked = [
                neighbour(buffers, x.checked_sub(1), Some(y)),
                neighbour(buffers, Some(x + 1), Some(y)),
                neighbour(buffers, Some(x), y.checked_sub(1)),
                neighbour(buffers, Some(x), Some(y + 1)),
            ]
            .into_iter()
            .any(|(id, there)| id != attr.opaque_id && here < there);
            if !marked {
                continue;
            }
            let rgb = edge_colors[usize::from(attr.opaque_id >> 3)];
            let pixel = &mut buffers.color[index];
            *pixel = color::pack(rgb, color::alpha(*pixel));
            if config.antialiasing {
                buffers.attrs[index].edge = MARKED_COVERAGE;
            }
        }
    }
}

/// Fog density, 0–128, at 24-bit `depth`: FOG_TABLE entries sit at
/// `FOG_OFFSET + FOG_STEP * (n + 1)`, with linear steps between them.
pub(crate) fn fog_density(depth: u32, config: &RenderConfig) -> u32 {
    let table = |n: usize| u32::from(config.fog_table[n]);
    let offset = config.fog_offset << 9;
    // FOG_STEP (0x400 >> FOG_SHIFT) in 24-bit depth. For shifts of 11 and
    // up, GBATEK expects a step of 0, so only the first and last entries;
    // melonDS measured this formula, which comes to nearly the same.
    let step = 0x8_0000 >> config.fog_shift;
    let relative = depth.saturating_sub(offset);
    let density = match (relative / step) as usize {
        0 => table(0),
        n @ 1..=31 => {
            let fraction = relative % step;
            (table(n - 1) * (step - fraction) + table(n) * fraction) / step
        }
        _ => table(31),
    };
    // 127 counts as full.
    if density >= 127 { 128 } else { density }
}

/// Blends fog into every pixel whose fog flag is set, and into the pixels
/// beneath anti-aliased edges, which those edges are blended with later.
pub(crate) fn apply_fog(buffers: &mut Buffers, config: &RenderConfig) {
    let fog_rgb = color::rgb6(config.fog_color);
    let fog_alpha = (config.fog_color >> 16) & 0x1F;
    let add_fog = |pixel: &mut u32, depth: u32| {
        let density = fog_density(depth, config);
        let mix = |fog: u32, value: u32| ((fog * density + value * (128 - density)) >> 7) as u8;
        let rgb = color::unpack(*pixel);
        let rgb = if config.fog_alpha_only {
            rgb
        } else {
            std::array::from_fn(|i| mix(u32::from(fog_rgb[i]), u32::from(rgb[i])))
        };
        *pixel = color::pack(rgb, mix(fog_alpha, u32::from(color::alpha(*pixel))));
    };
    let Buffers {
        color: pixels,
        depth,
        attrs,
        ..
    } = buffers;
    let (top, beneath) = pixels.split_at_mut(PIXELS);
    for (index, pixel) in top.iter_mut().enumerate() {
        let attr = attrs[index];
        if attr.fog {
            add_fog(pixel, depth[index]);
        }
        let below = index + PIXELS;
        if attr.coverage() < 31 && attrs[below].fog {
            add_fog(&mut beneath[index], depth[below]);
        }
    }
}

/// Blends each anti-aliased edge with the pixel beneath it by the edge's
/// coverage. Over a transparent pixel, the edge keeps its colour and only
/// its alpha fades.
pub(crate) fn antialias(buffers: &mut Buffers) {
    let (top, beneath) = buffers.color.split_at_mut(PIXELS);
    for ((pixel, &under), attr) in top.iter_mut().zip(&*beneath).zip(&buffers.attrs[..PIXELS]) {
        let coverage = attr.coverage();
        if coverage == 31 {
            continue;
        }
        let weight = u32::from(coverage) + 1;
        let mix = |over: u8, under: u8| {
            ((u32::from(over) * weight + u32::from(under) * (32 - weight)) >> 5) as u8
        };
        let (over_rgb, under_rgb) = (color::unpack(*pixel), color::unpack(under));
        let rgb = if color::alpha(under) == 0 {
            over_rgb
        } else {
            std::array::from_fn(|i| mix(over_rgb[i], under_rgb[i]))
        };
        *pixel = color::pack(rgb, mix(color::alpha(*pixel), color::alpha(under)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu3d::registers::RenderRegisters;

    fn config(fog_offset: u16, shift: u16, table: &[(u32, u8)]) -> RenderConfig {
        let mut registers = RenderRegisters::default();
        registers.write_control(shift << 8, 0xF00);
        let [low, high] = fog_offset.to_le_bytes();
        registers.write_byte(0x35C, low);
        registers.write_byte(0x35D, high);
        for &(index, value) in table {
            registers.write_byte(0x360 + index, value);
        }
        registers.config()
    }

    #[test]
    fn fog_density_steps_through_the_table() {
        // Step 0x400 >> 2 = 0x100 (15-bit depth); entry n sits at
        // 0x1000 + 0x100 * (n + 1).
        let config = config(0x1000, 2, &[(0, 0), (1, 64), (31, 127)]);
        assert_eq!(fog_density(0x0800 << 9, &config), 0, "before the offset");
        assert_eq!(fog_density(0x1100 << 9, &config), 0, "at entry 0");
        assert_eq!(fog_density(0x1180 << 9, &config), 32, "halfway to entry 1");
        assert_eq!(fog_density(0x1200 << 9, &config), 64, "at entry 1");
        assert_eq!(fog_density(0x7FFF << 9, &config), 128, "127 counts as 128");
    }

    #[test]
    fn fog_steps_shrink_past_shift_ten() {
        let config = config(0x1000, 15, &[(0, 0), (31, 64)]);
        assert_eq!(fog_density(0x1000 << 9, &config), 0);
        assert_eq!(fog_density(0x1001 << 9, &config), 64);
    }

    const RED: u32 = 63 | 31 << 18;
    const BLUE: u32 = 63 << 12 | 31 << 18;

    #[test]
    fn antialiasing_blends_edges_with_the_pixel_beneath() {
        let mut buffers = Buffers::default();
        for (index, coverage, beneath) in [(0, 15, BLUE), (1, 7, 0), (2, 31, BLUE)] {
            buffers.color[index] = RED;
            buffers.attrs[index].edge = coverage;
            buffers.color[index + PIXELS] = beneath;
        }
        antialias(&mut buffers);
        assert_eq!(
            buffers.color[0],
            color::pack([31, 0, 31], 31),
            "half and half"
        );
        assert_eq!(
            buffers.color[1],
            color::pack([63, 0, 0], 7),
            "a quarter, over a transparent pixel"
        );
        assert_eq!(buffers.color[2], RED, "whole");
    }

    #[test]
    fn fog_reaches_the_pixels_beneath_antialiased_edges() {
        // Full black, transparent fog at every depth.
        let config = config(0, 0, &[(0, 127)]);
        let mut buffers = Buffers::default();
        for (index, coverage) in [(0, 15), (1, 31)] {
            buffers.color[index] = RED;
            buffers.attrs[index].edge = coverage;
            buffers.color[index + PIXELS] = RED;
            buffers.attrs[index + PIXELS].fog = true;
        }
        apply_fog(&mut buffers, &config);
        assert_eq!(buffers.color[0], RED, "the edge has no fog flag");
        assert_eq!(buffers.color[PIXELS], 0, "beneath the edge");
        assert_eq!(buffers.color[1 + PIXELS], RED, "beneath a whole pixel");
    }
}
