//! Whole-frame passes after the polygons are drawn: edge marking, fog and
//! anti-aliasing (GBATEK, "DS 3D Toon, Edge, Fog, Alpha-Blending,
//! Anti-Aliasing").

use super::pixel::Buffers;
use crate::gpu3d::color;
use crate::gpu3d::registers::RenderConfig;

/// An anti-aliased edge that edge marking paints is blended about half
/// with the pixel beneath, whatever it covered (GBATEK guesses "alpha=16
/// or so"; melonDS uses this).
const MARKED_COVERAGE: u8 = 16;

/// Left, right, up and down, as steps across and down.
const NEIGHBOURS: [(isize, isize); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

/// Paints the edges of opaque polygons in the edge colour for their
/// polygon ID: where a neighbour (up, down, left, right) has another ID
/// and is further away. Beyond the buffers' area, neighbours are the rear
/// plane's ID and depth.
///
/// Above the native resolution, an outline is as thick as a screen pixel:
/// the paint runs on from each marked pixel away from the neighbour that
/// marked it, over its own polygon ID, for `scale` pixels in all.
pub(crate) fn mark_edges(buffers: &mut Buffers, config: &RenderConfig) {
    let clear_id = ((config.clear_color >> 24) & 0x3F) as u8;
    let edge_colors = config.edge_colors.map(|c| color::rgb6(u32::from(c)));
    let area = buffers.area;
    let (width, rows) = (area.width(), area.rows);
    // The pixel `steps` steps along `(dx, dy)` from (x, y), if inside.
    let step = |x: usize, y: usize, (dx, dy): (isize, isize), steps: isize| {
        let x = x.checked_add_signed(dx * steps)?;
        let y = y.checked_add_signed(dy * steps)?;
        (x < width && y < rows).then_some(y * width + x)
    };
    for y in 0..rows {
        for x in 0..width {
            let index = y * width + x;
            let attr = buffers.attrs[index];
            if !attr.on_edge() {
                continue;
            }
            let here = buffers.depth[index];
            let marked_by = NEIGHBOURS.map(|direction| {
                let (id, there) = step(x, y, direction, 1)
                    .map_or((clear_id, config.clear_depth), |neighbour| {
                        (buffers.attrs[neighbour].opaque_id, buffers.depth[neighbour])
                    });
                id != attr.opaque_id && here < there
            });
            if !marked_by.contains(&true) {
                continue;
            }
            let rgb = edge_colors[usize::from(attr.opaque_id >> 3)];
            let paint = |pixel: &mut u32| *pixel = color::pack(rgb, color::alpha(*pixel));
            paint(&mut buffers.color[index]);
            if config.antialiasing {
                buffers.attrs[index].edge = MARKED_COVERAGE;
            }
            for (direction, _) in NEIGHBOURS.into_iter().zip(marked_by).filter(|(_, by)| *by) {
                for steps in 1..area.scale as isize {
                    let Some(inner) = step(x, y, direction, -steps) else {
                        break;
                    };
                    if buffers.attrs[inner].opaque_id != attr.opaque_id {
                        break;
                    }
                    paint(&mut buffers.color[inner]);
                }
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
        layer,
        ..
    } = buffers;
    let (top, beneath) = pixels.split_at_mut(*layer);
    for (index, pixel) in top.iter_mut().enumerate() {
        let attr = attrs[index];
        if attr.fog {
            add_fog(pixel, depth[index]);
        }
        let below = index + *layer;
        if attr.coverage() < 31 && attrs[below].fog {
            add_fog(&mut beneath[index], depth[below]);
        }
    }
}

/// Blends each anti-aliased edge with the pixel beneath it by the edge's
/// coverage. Over a transparent pixel, the edge keeps its colour and only
/// its alpha fades.
pub(crate) fn antialias(buffers: &mut Buffers) {
    let layer = buffers.layer;
    let (top, beneath) = buffers.color.split_at_mut(layer);
    for ((pixel, &under), attr) in top.iter_mut().zip(&*beneath).zip(&buffers.attrs[..layer]) {
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
            buffers.color[index + buffers.layer] = beneath;
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
            buffers.color[index + buffers.layer] = RED;
            buffers.attrs[index + buffers.layer].fog = true;
        }
        apply_fog(&mut buffers, &config);
        assert_eq!(buffers.color[0], RED, "the edge has no fog flag");
        assert_eq!(buffers.color[buffers.layer], 0, "beneath the edge");
        assert_eq!(
            buffers.color[1 + buffers.layer],
            RED,
            "beneath a whole pixel"
        );
    }
}
